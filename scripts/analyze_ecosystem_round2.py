"""Source-backed OHLCV, liquidity, actual original activity and paired impacts."""
from __future__ import annotations
import argparse, collections, hashlib, json, math
from pathlib import Path
import numpy as np
import matplotlib.pyplot as plt
from matplotlib.patches import Rectangle
from plot_strategy_market import candles, export_csv, UP, DOWN
from research_ecosystem_round2 import OUT, CASES
CHARTS=OUT/'charts'

def read(p):return json.loads(p.read_text(encoding='utf-8'))
def q(values):
    return np.quantile(values,[.1,.5,.9]).tolist() if values else None
def book_row(s):
    b,a=s['book']['bids'],s['book']['asks']
    if not b or not a:return {'time_ms':s['time_ms'],'mid':None,'spread':None,'bid_depth':None,'ask_depth':None}
    bid=max(x['price_tick'] for x in b);ask=min(x['price_tick']for x in a);mid=(bid+ask)/2
    return {'time_ms':s['time_ms'],'mid':mid,'spread':(ask-bid)/mid*10000,
        'bid_depth':sum(x['qty']for x in b if x['price_tick']>=mid*.9975),
        'ask_depth':sum(x['qty']for x in a if x['price_tick']<=mid*1.0025)}
def sign_runs(changes):
    previous=0;current=longest=0
    for value in changes:
        sign=np.sign(value)
        current=current+1 if sign and sign==previous else (1 if sign else 0)
        longest=max(longest,current);previous=sign
    return longest
def analyze(r,roster):
    owners={p.get('account_id',100+i):p for i,p in enumerate(roster['participants'])}
    by_id={p['participant_id']:p for p in roster['participants']}
    qty=collections.Counter();original_qty=collections.Counter();period_qty=collections.Counter();original_taker_qty=0
    for t in r['trade_receipts']:
        p=owners.get(t['taker_account_id'])
        if p:
            qty[p['role']]+=t['qty']
            if (p['family']or'').startswith('Original:'):original_taker_qty+=t['qty']
        for account in [t['maker_account_id'],t['taker_account_id']]:
            p=owners.get(account)
            if p and p['role']=='technical':
                period_qty[p['config']['bar_interval_ms']]+=t['qty']
                if (p['family']or'').startswith('Original:'):original_qty[p['family']]+=t['qty']
    result={'raw_trades':r['trades'],'active_instances':len(r['fills']),
        'taker_qty':dict(qty),'taker_qty_shares':{k:v/sum(qty.values())for k,v in qty.items()},
        'original_participation_qty':dict(original_qty),'original_sources_with_actual_fills':len(original_qty),
        'technical_participation_by_period_ms':dict(period_qty),'markets':{},'states_by_period':{}}
    result['original_sources_instanced']=sorted({p['family'] for p in roster['participants'] if (p['family']or'').startswith('Original:')})
    result['original_sources_instanced_count']=len(result['original_sources_instanced'])
    result['original_taker_qty_share']=original_taker_qty/sum(qty.values()) if qty else None
    result['native_technical_families_instanced_count']=len({p['family'] for p in roster['participants'] if p['role']=='technical' and not (p['family']or'').startswith('Original:')})
    states=collections.defaultdict(list);maker_states=[]
    for saved in r['states']:
        p=by_id[saved['id']];s=saved['state']['Plugin']['data']
        if p['role']=='maker':maker_states.append(s)
        if p['role']=='technical':
            states[p['config']['bar_interval_ms']].append({'id':p['participant_id'],'original':(p['family']or'').startswith('Original:'),
                'ready':s.get('frame',{}).get('ready'),'bars':s.get('evaluated_bars',s.get('bars')) if isinstance(s.get('bars'),int) else len(s.get('bars',[])),
                'filled':p['participant_id'] in r['fills']})
    for period,rows in states.items():
        result['states_by_period'][period]={'instances':len(rows),'native_ready':sum(bool(s['ready'])for s in rows),
            'original_instances':sum(s['original']for s in rows),'with_actual_fills':sum(s['filled']for s in rows),
            'min_bars':min(s['bars']or 0 for s in rows),'max_bars':max(s['bars']or 0 for s in rows)}
    result['adaptive_makers_with_successful_proxy_fit']=sum(bool(s.get('calibrated'))for s in maker_states)
    for leg in ('SPOT','PERP'):
        inst='V-BTC-'+leg;bars=candles(r,inst,60000)
        duration_hours=r['simulation_time_ms']/3600000
        selected=[b for b in bars if b['time_ms']>=max(0,duration_hours-6)*3600000 and not b['partial']]
        closes=np.array([b['close']for b in selected],dtype=float);returns=np.diff(np.log(closes)) if len(closes)>1 else np.array([])
        changes=np.diff(closes)
        books=[book_row(s)for s in r['book_snapshots']if s['instrument_id']==inst]
        valid=[b for b in books if b['mid']is not None]
        signs=[];previous=None
        for t in r['trade_receipts']:
            if t['instrument_id']!=inst:continue
            key=(t['time_ms'],t['taker_order_id'])
            if key!=previous:signs.append(t['taker_side']);previous=key
        result['markets'][leg]={'book_samples':len(books),'two_sided_fraction':len(valid)/len(books),
            'spread_bps_p10_p50_p90':q([b['spread']for b in valid]),
            'bid_depth_25bps_p10_p50_p90':q([b['bid_depth']for b in valid]),'ask_depth_25bps_p10_p50_p90':q([b['ask_depth']for b in valid]),
            'measurement_window':f'last {min(6,duration_hours):g} simulated hours; nonempty closed one-minute candles',
            'one_minute_bars':len(selected),'longest_one_minute_close_same_sign_run':int(sign_runs(changes)),
            'flat_one_minute_close_change_fraction':float(np.mean(changes==0)) if len(changes)else None,
            'one_minute_return_std_bps':float(np.std(returns)*10000)if len(returns)else None,
            'filled_taker_orders':len(signs),'filled_order_same_direction_fraction':sum(a==b for a,b in zip(signs,signs[1:]))/(len(signs)-1)if len(signs)>1 else None}
        export_csv(CHARTS/(roster['case']+'-'+leg.lower()+'-1m.csv'),bars)
        export_csv(CHARTS/(roster['case']+'-'+leg.lower()+'-books.csv'),books)
    return result

def draw(ax,bars,interval_ms,start=0,end=12):
    width=interval_ms/3600000*.72
    for b in bars:
        if b['time_ms']/3600000<start:continue
        x=(b['time_ms']+interval_ms/2)/3600000;color=UP if b['close']>=b['open']else DOWN
        ax.vlines(x,b['low'],b['high'],color=color,linewidth=.7)
        height=abs(b['close']-b['open']);bottom=min(b['open'],b['close'])
        if height:ax.add_patch(Rectangle((x-width/2,bottom),width,height,facecolor=color,edgecolor=color,linewidth=.3,alpha=.35 if b['partial']else 1))
        else:ax.hlines(bottom,x-width/2,x+width/2,color=color,linewidth=.8)
    ax.set_xlim(start,end+.06);ax.set_ylabel('价格（内部 tick）');ax.set_xlabel('仿真时间（小时）')

def plot_single(name,r,stats):
    end=r['simulation_time_ms']/3600000;start=max(0,end-2)
    fig,axes=plt.subplots(2,2,figsize=(16,8),height_ratios=[3,1],layout='constrained')
    for col,leg in enumerate(('SPOT','PERP')):
        bars=candles(r,'V-BTC-'+leg,60000);draw(axes[0,col],bars,60000,start=start,end=end)
        axes[0,col].set_title('现货 · 1 分钟 K 线'if leg=='SPOT'else'永续 · 1 分钟 K 线')
        chosen=[b for b in bars if b['time_ms']>=start*3600000]
        axes[1,col].bar([(b['time_ms']+30000)/3600000 for b in chosen],[b['volume']for b in chosen],width=.012,
            color=[UP if b['close']>=b['open']else DOWN for b in chosen]);axes[1,col].set_xlim(start,end+.06)
        axes[1,col].set_ylabel('成交量（内部 lot）');axes[1,col].set_xlabel('仿真时间（小时）')
    fig.suptitle(f'{r["bots"]} bot · {name} · {end:g} 小时运行的最后 2 小时\n原版 Pine 实际成交 {stats["original_sources_with_actual_fills"]}/{stats["original_sources_instanced_count"]} 个入场源码；长周期由真实生成的历史预热。',fontsize=15)
    fig.supxlabel(f'价格、数量为内部单位；无成交时段不补造 bar；每 {r["clock_step_seconds"]} 秒决策波，不是实盘延迟资格。',fontsize=10)
    fig.savefig(CHARTS/(name+'-candles.png'),dpi=145);plt.close(fig)

def comparison(runs):
    keys=['legacy-n5-s7','adaptive-n0-s7','adaptive-n5-s7','adaptive-n10-s7']
    titles=['旧做市 + 5% 噪声','自适应做市 + 0% 噪声','自适应做市 + 5% 噪声','自适应做市 + 10% 噪声']
    if not all(k in runs for k in keys):
        keys=['legacy-n5-s7','adaptive-n5-s7'];titles=['旧做市 + 5% 噪声','自适应做市 + 5% 噪声']
        if not all(k in runs for k in keys):return
    assert len({runs[k]['simulation_time_ms']for k in keys})==1
    end=runs[keys[0]]['simulation_time_ms']/3600000
    fig,axes=plt.subplots(len(keys),2,figsize=(16,3.5*len(keys)),layout='constrained')
    for col,leg in enumerate(('SPOT','PERP')):
        all_bars=[]
        for row,(key,title)in enumerate(zip(keys,titles)):
            bars=candles(runs[key],'V-BTC-'+leg,300000);all_bars+=bars
            draw(axes[row,col],bars,300000,end=end);axes[row,col].set_title(title+' · '+('现货'if leg=='SPOT'else'永续'))
        low=min(b['low']for b in all_bars);high=max(b['high']for b in all_bars)
        for row in range(len(keys)):axes[row,col].set_ylim(low-10,high+10)
    assert len({runs[k]['clock_step_seconds'] for k in keys})==1
    cadence=runs[keys[0]]['clock_step_seconds']
    count=runs[keys[0]]['bots']
    assert len({runs[k]['bots'] for k in keys})==1
    fig.suptitle(f'{count} bot · 种子 7 · 每 {cadence} 秒决策 · 真实成交的 5 分钟 K 线\n每列共同价格轴；噪声替代部分技术人口，资金分配也随配比变化。',fontsize=15)
    fig.savefig(CHARTS/'maker-noise-candles.png',dpi=135);plt.close(fig)
    fig,axes=plt.subplots(2,2,figsize=(16,8),layout='constrained')
    for col,leg in enumerate(('SPOT','PERP')):
        for key,label,color in [('legacy-n5-s7','旧做市','#335f9d'),('adaptive-n5-s7','自适应做市','#bd6828')]:
            bins=collections.defaultdict(list)
            for s in runs[key]['book_snapshots']:
                if s['instrument_id']=='V-BTC-'+leg:
                    b=book_row(s)
                    if b['mid']is not None:bins[s['time_ms']//60000].append(b)
            t=[k/60 for k in sorted(bins)]
            for row,field in enumerate(('spread','depth')):
                vals=[[b['spread']if field=='spread'else b['bid_depth']+b['ask_depth']for b in bins[k]]for k in sorted(bins)]
                med=[np.median(v)for v in vals];lo=[np.quantile(v,.1)for v in vals];hi=[np.quantile(v,.9)for v in vals]
                axes[row,col].plot(t,med,label=label,color=color,linewidth=1)
                axes[row,col].fill_between(t,lo,hi,color=color,alpha=.12)
                axes[row,col].set_xlabel('仿真时间（小时）');axes[row,col].set_xlim(0,12)
        axes[0,col].set_title('现货'if leg=='SPOT'else'永续');axes[0,col].set_ylabel('价差（bps）')
        axes[1,col].set_ylabel('中价附近 25 bps 双边深度（lot）');axes[0,col].legend()
    fig.suptitle('相同非做市人口、资金和库存下的盘口对照\n线：每分钟样本中位数；阴影：分钟内 P10–P90，非置信区间。',fontsize=15)
    fig.savefig(CHARTS/'maker-books.png',dpi=145);plt.close(fig)
    fig,axes=plt.subplots(2,2,figsize=(15,9),layout='constrained')
    for row,(key,label) in enumerate([('legacy-n5-s7','旧做市'),('adaptive-n5-s7','自适应做市')]):
        for col,leg in enumerate(('SPOT','PERP')):
            snapshot=[s for s in runs[key]['book_snapshots'] if s['instrument_id']=='V-BTC-'+leg][-1]
            ax=axes[row,col];book=snapshot['book']
            chosen={side:sorted(book[side],key=lambda b:b['price_tick'],reverse=side=='bids')[:12]for side in ['bids','asks']}
            prices=sorted({b['price_tick']for levels in chosen.values()for b in levels});positions={price:i for i,price in enumerate(prices)}
            if not prices:
                ax.text(.5,.5,'此采样时点没有挂单',transform=ax.transAxes,ha='center',va='center',fontsize=15,color='#777777')
            for side,color,direction in [('bids',UP,-1),('asks',DOWN,1)]:
                levels=chosen[side]
                ax.barh([positions[b['price_tick']]for b in levels],[direction*b['qty']for b in levels],
                    color=color,alpha=.8,label='买盘'if side=='bids'else'卖盘')
            ax.set_yticks(range(len(prices)),[str(price)for price in prices],fontsize=8)
            ax.set_title(label+' · '+('现货'if leg=='SPOT'else'永续'))
            ax.axvline(0,color='#888888',linewidth=.6);ax.set_xlabel('每档实际数量：左买 / 右卖（lot）')
            ax.set_ylabel('实际报价（tick；各图价格独立）')
            if prices:ax.legend(fontsize=9)
    fig.suptitle(f'{end:g} 小时末的真实盘口 · 两侧各展示最近 12 档\n这是采样时点的挂单，不能证明波内始终有双边报价。',fontsize=15)
    fig.savefig(CHARTS/'maker-book-levels.png',dpi=145);plt.close(fig)

def impacts():
    folders=list(OUT.glob('impact-*'));folder=folders[0] if folders else OUT/'impact-300'
    files=list(folder.glob('V-BTC-*.json'))
    if len(files)!=14:return None
    runs={p.stem:read(p)for p in files};results=[]
    # Both independently restored no-intervention branches must be identical.
    assert runs['V-BTC-SPOT-Buy-0']['trade_receipts']==runs['V-BTC-PERP-Buy-0']['trade_receipts']
    fig,axes=plt.subplots(2,2,figsize=(16,8),layout='constrained')
    for col,leg in enumerate(('SPOT','PERP')):
        inst='V-BTC-'+leg;control=runs[inst+'-Buy-0']
        baseline={s['time_ms']:book_row(s)for s in control['book_snapshots']if s['instrument_id']==inst}
        before=book_row({'book':control['before_book'],'time_ms':control['book_snapshots'][0]['time_ms']-control['clock_step_seconds']*1000})
        baseline[before['time_ms']]=before;start=before['time_ms']
        for quantity,color in [(10,'#335f9d'),(100,'#bd6828'),(1000,'#883b91')]:
            for side,style in [('Buy','-'),('Sell','--')]:
                r=runs[inst+'-'+side+'-'+str(quantity)];inter=r['intervention']
                rows=[book_row({'book':r['immediate_book'],'time_ms':start})]+[book_row(s)for s in r['book_snapshots']if s['instrument_id']==inst]
                points=[]
                for b in rows:
                    c=baseline[b['time_ms']]
                    depth='ask_depth'if side=='Buy'else'bid_depth'
                    residual=(b['mid']-c['mid'])/before['mid']*10000 if b['mid']is not None and c['mid']is not None else math.nan
                    depletion=b[depth]-c[depth] if b[depth]is not None and c[depth]is not None else math.nan
                    points.append(((b['time_ms']-start)/1000,residual,depletion))
                x=[p[0]for p in points];price=[p[1]for p in points];depth=[p[2]for p in points]
                label=('买'if side=='Buy'else'卖')+f' {quantity}'
                axes[0,col].plot(x,price,color=color,linestyle=style,label=label,linewidth=1)
                axes[1,col].plot(x,depth,color=color,linestyle=style,linewidth=1)
                vwap=int(inter['notional'])/inter['filled_qty']if inter['filled_qty']else None
                finite_price=[p for p in price if math.isfinite(p)]
                results.append({'instrument':inst,'side':side,'requested_qty':quantity,'filled_qty':inter['filled_qty'],
                    'cost_from_pre_mid_bps':(vwap-before['mid'])/before['mid']*10000*(1 if side=='Buy'else-1)if vwap else None,
                    'immediate_two_sided':rows[0]['mid']is not None,
                    'max_observed_abs_mid_residual_bps':max(abs(p)for p in finite_price)if finite_price else None,
                    'end_mid_residual_bps':price[-1]if price and math.isfinite(price[-1]) else None,
                    'observed_mid_samples':len(finite_price),'total_mid_samples':len(price),
                    'residual_definition':'intervention minus no-intervention branch; conditional on two-sided books; same frozen parent'})
        axes[0,col].set_title('现货'if leg=='SPOT'else'永续');axes[0,col].set_ylabel('相对控制分支的中价差（bps）')
        axes[1,col].set_ylabel('被吃一侧近端深度差（lot）');axes[0,col].legend(ncol=3,fontsize=9)
        for ax in axes[:,col]:ax.set_xlabel('订单提交后（仿真秒）');ax.axhline(0,color='#888888',linewidth=.6)
    fig.suptitle('真实订单冲击：同一预热状态分别提交，随后让所有策略继续交易\n10 / 100 / 1,000 lot；买卖独立分支；仅绘制双边有报价时的中价。',fontsize=15)
    fig.savefig(CHARTS/f'impact-{control["bots"]}.png',dpi=145);plt.close(fig)
    return {'control_branches_identical':True,'sample_seconds':control['clock_step_seconds'],'followup_seconds':600,'orders':results}

def main():
    global OUT,CHARTS
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--output-dir',type=Path);a=p.parse_args()
    if a.output_dir:OUT=a.output_dir.resolve();CHARTS=OUT/'charts'
    CHARTS.mkdir(exist_ok=True)
    runs={};report={'runs':[],'qualification':'controlled synthetic matching; no real-market or minimum-population qualification'}
    for name,count,seed,noise,maker in CASES:
        path=OUT/(name+'.receipt.json')
        if not path.exists()or not path.stat().st_size:
            parents=list(OUT.glob('impact-*/parent.json')) if name=='adaptive-n5-s7' else []
            if not parents:continue
            path=parents[0]
        try:r=read(path)
        except json.JSONDecodeError:continue
        if not r.get('run_complete',True):continue
        roster=read(OUT/(name+'.roster.json'));roster['case']=name
        assert sum(r['trades'].values())==len(r['trade_receipts'])
        keys=[(t['instrument_id'],t['trade_id'])for t in r['trade_receipts']];assert len(keys)==len(set(keys))
        stats=analyze(r,roster);runs[name]=r
        row={'case':name,'count':roster['count'],'simulation_hours':r['simulation_time_ms']/3600000,'clock_step_seconds':r['clock_step_seconds'],'seed':seed,'noise_fraction':noise,'maker':maker,
            'receipt_path':str(path),'receipt_sha256':hashlib.sha256(path.read_bytes()).hexdigest(),**stats}
        report['runs'].append(row);plot_single(name,r,stats)
    comparison(runs);report['impact']=impacts()
    primary=['legacy-n5-s7','adaptive-n0-s7','adaptive-n5-s7','adaptive-n10-s7']
    report['primary_comparison_complete']=all(k in runs for k in primary) and report['impact']is not None
    report['complete']=len(runs)==len(CASES) and report['impact']is not None and all(runs[n]['bots']==count for n,count,*_ in CASES)
    (CHARTS/'report.json').write_text(json.dumps(report,ensure_ascii=False,indent=2,allow_nan=False)+'\n',encoding='utf-8')
    print(json.dumps({'complete':report['complete'],'cases':[r['case']for r in report['runs']],
        'metrics':[{k:r[k]for k in ['case','active_instances','original_sources_with_actual_fills','taker_qty_shares','markets']}for r in report['runs']]},ensure_ascii=True,indent=2))
if __name__=='__main__':main()
