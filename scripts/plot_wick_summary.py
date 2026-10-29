"""Plot every full-study seed; historical ranges are descriptive, not intervals."""
import json
import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from research_wick_observations import BASE


def plot():
    out=BASE/'full-dual-retry';data=json.loads((out/'full-comparison.json').read_text(encoding='utf-8'));assert data['complete']
    reference=json.loads((BASE/'diagnosis-v2/anatomy-summary.json').read_text(encoding='utf-8'))['reference_windows']
    return_reference=json.loads((BASE/'diagnosis-v2/reference-return-summary.json').read_text(encoding='utf-8'))['summary']
    fig,axes=plt.subplots(2,3,figsize=(15,8));plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10})
    plots=[('wick_1m','1m wick fraction',60000),('wick_5m','5m wick fraction',300000),
        ('both_outer_quarters_5m','5m both-long-wick frequency',300000),('acf1','1m return lag1 correlation',None),
        ('std_bp','1m return standard deviation (bp)',None),('volume_ratio','Actual primary-window volume / control',None)]
    colors=['#416c88','#71956b'];markers=['o','s','^','D'];seeds=(41,7,19,73)
    for ax,(key,label,interval) in zip(axes.flat,plots):
        for leg,market in enumerate(('V-BTC-SPOT','V-BTC-PERP')):
            offset=leg*3
            if interval:
                refs=[r for r in reference if r['market']==market and r['interval_minutes']==interval//60000]
                vals=[r['shape']['both_outer_quarters_fraction'] if key.startswith('both_') else r['shape']['median']['wick_fraction']for r in refs]
                ax.fill_between([offset-.4,offset+1.4],min(vals),max(vals),color='#d8d8d8',alpha=.65,zorder=0)
            if key=='std_bp':
                ref=next(r for r in return_reference if r['market']==market)
                ax.fill_between([offset-.4,offset+1.4],ref['std_bp_min'],ref['std_bp_max'],color='#d8d8d8',alpha=.65,zorder=0)
            for col,profile in enumerate(('control','dual-execution')):
                values=[]
                for i,seed in enumerate(seeds):
                    r=next(r for r in data['rows']if r['market']==market and r['seed']==seed and r['profile']==profile)
                    if key=='volume_ratio':
                        control=next(v for v in data['rows']if v['market']==market and v['seed']==seed and v['profile']=='control');v=r['volume']/control['volume']
                    else:v=r[key]
                    values.append(v);ax.scatter(offset+col+(i-1.5)*.045,v,color=colors[col],marker=markers[i],s=35,zorder=3)
                ax.hlines(np.mean(values),offset+col-.25,offset+col+.25,color=colors[col],lw=2,zorder=2)
        ax.set_title(label);ax.set_xticks([0,1,3,4],['SPOT\ncontrol','SPOT\nexecution','PERP\ncontrol','PERP\nexecution']);ax.grid(axis='y',alpha=.2)
        ax.spines[['right','top']].set_visible(False)
        if interval:ax.set_ylim(0,1)
        if key=='acf1':ax.axhline(0,color='#888',lw=.8,ls=':');ax.set_ylim(-1,1)
        if key=='volume_ratio':ax.axhline(1,color='#888',lw=.8,ls=':');ax.set_ylim(bottom=0)
        if key=='std_bp':ax.set_ylim(bottom=0)
    handles=[plt.Line2D([0],[0],marker=m,color='#666',linestyle='',label=f'seed {s}')for m,s in zip(markers,seeds)]
    fig.legend(handles=handles,loc='upper center',ncol=4,bbox_to_anchor=(.5,.955))
    fig.suptitle('Wick mechanism | all four full12h outcomes | primary hours2–12',fontsize=15,y=.99)
    fig.text(.5,.015,'Symbols = individual fixed seeds; horizontal strokes = arithmetic mean of four seed metrics. No uncertainty intervals.\nGrey = min–max of eight cached10h BTCUSDT windows per market, descriptive only. All numeric recipe inputs held fixed; execution changes.',ha='center')
    fig.tight_layout(rect=[0,.06,1,.92]);fig.savefig(out/'wick-statistics.png',dpi=150);fig.savefig(out/'wick-statistics.svg');plt.close(fig)


if __name__=='__main__':plot()
