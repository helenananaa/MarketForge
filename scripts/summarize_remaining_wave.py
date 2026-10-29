"""Compare the frozen early residual-wave trace, without reselecting a window."""
import csv,json
from collections import Counter,defaultdict
from pathlib import Path
from analyze_market_mechanisms import ROOT,sha,write


def rows(path):
    with path.open(encoding='utf-8') as f:return list(csv.DictReader(f))


def summarize():
    root=ROOT/'.local/strategy-research-causal-20261025';result=[];series={}
    for profile,folder,innovation in [('depth','remaining-depth-analysis-v3',True),('level','inventory-level-pilot-analysis-v3',False)]:
        directory=root/folder;makers=rows(directory/'maker-decisions.csv');basis=rows(directory/'basis-decisions.csv');trades=rows(directory/'trades.csv')
        trace=json.loads((directory/'trace-summary.json').read_text(encoding='utf-8'))
        assert trace['analyzer_schema']==3 and trace['maker_center_max_reconstruction_error']<1e-6
        # Actual rests distinguish displayed orders from rejected/only-cancel proposals.
        gateways=json.loads((directory/'gateway-accounts.json').read_text(encoding='utf-8'));rested=Counter()
        for g in gateways:rested[(g['time_ms'],g['participant'])]+=g['rested']
        del gateways
        previous={};jumps=[]
        for r in makers:
            if r['reconsidered']!='True':continue
            r=dict(r);raw=float(r['inventory_bias'])+float(r['reference_bias'])
            r['private_before_final_clamp']=float(r['reservation_offset'])-float(r['previous_offset']) if innovation else raw
            r['rested_orders']=rested[(int(r['time_ms']),r['participant'])]
            old=previous.get(r['participant'])
            if old and r['rested_orders'] and old['rested_orders']:
                jumps.append(dict(participant=r['participant'],from_ms=int(old['time_ms']),to_ms=int(r['time_ms']),
                    center_change=float(r['center'])-float(old['center']),mid_change=float(r['mid'])-float(old['mid']),
                    private_change=r['private_before_final_clamp']-old['private_before_final_clamp'],flow_change=float(r['flow_impact'])-float(old['flow_impact']),
                    inventory_from=int(old['position']),inventory_to=int(r['position']),from_row=old,to_row=r))
            previous[r['participant']]=r
        chosen=[r for r in makers if r['participant']=='maker-spot-4'];series[profile]=chosen
        role=Counter()
        for r in trades:role[r['taker_role']]+=int(r['qty'])
        lead=[r for r in basis if r['leg']=='Perp'];entries=[r for r in lead if r['lead_entry']=='True']
        result.append(dict(profile=profile,trace_sha256=trace['trace_sha256'],source_hashes={p.name:sha(p) for p in directory.iterdir() if p.suffix=='.csv' or p.name=='trace-summary.json'},
            trades=len(trades),role_taker_qty=dict(role),entry_proposals=len(entries),entries_below_threshold=trace['unexecutable_at_configured_threshold'],
            entries_while_unhedged=sum(int(r['residual'])!=0 for r in entries),pair_seconds=len(lead),unhedged_pair_seconds=sum(int(r['residual'])!=0 for r in lead),
            mean_absolute_pre_wave_residual=sum(abs(int(r['residual'])) for r in lead)/len(lead),max_absolute_pre_wave_residual=max(abs(int(r['residual'])) for r in lead),
            liquidation_trades=trace['liquidation_trades'],maker4_inventory_range=[min(int(r['position']) for r in chosen),max(int(r['position']) for r in chosen)],
            largest_confirmed_reconsideration_jumps=sorted(jumps,key=lambda r:abs(r['center_change']),reverse=True)[:8]))
    write(root/'remaining-window-comparison.json',dict(window_ms=[10620000,11280000],records=result,
        scope='Same retrospectively selected early window under both treatments. Depth and level use the identical frozen executable. Rested orders use authoritative gateway events. Delta components are pre-clamp contributions; a final clamp can make their sum differ from actual center change. Level restores both inventory levels and the legacy raw-plus-flow clamp ordering. Pair residuals here are pre-wave observations; the original window has a separate actual gateway position ledger. This pilot neither selects retained seeds nor qualifies full-run realism.'))
    import matplotlib
    matplotlib.use('Agg')
    import matplotlib.pyplot as plt
    plt.rcParams.update({'font.family':'DejaVu Sans','font.size':10,'axes.spines.top':False,'axes.spines.right':False})
    fig,axes=plt.subplots(5,2,figsize=(14,12),sharex=True,sharey='row')
    for col,profile in enumerate(('depth','level')):
        rs=series[profile];x=[int(r['time_ms'])/60000 for r in rs]
        axes[0,col].plot(x,[float(r['book_mid']) if r['book_mid'] else float('nan') for r in rs],color='#1f4e79',lw=.8)
        axes[1,col].plot(x,[float(r['center']) for r in rs],color='#5b7e40',lw=.8,label='Computed center')
        axes[1,col].plot(x,[float(r['mid']) for r in rs],color='#1f4e79',lw=.7,label='External reference')
        axes[2,col].plot(x,[int(r['position']) for r in rs],color='#67517d',lw=.8);axes[2,col].axhline(216,color='#777',ls=':',label='Target 216')
        selected=[r for r in rs if r['reconsidered']=='True'];t=[int(r['time_ms'])/60000 for r in selected]
        private=[float(r['reservation_offset'])-float(r['previous_offset']) if profile=='depth' else float(r['inventory_bias'])+float(r['reference_bias']) for r in selected]
        axes[3,col].plot(t,private,color='#a34d3f',lw=.8)
        axes[4,col].plot(t,[float(r['flow_impact']) for r in selected],color='#b46b25',lw=.8)
        axes[0,col].set_title('Depth baseline: differenced bias' if profile=='depth' else 'Inventory level: frozen pilot')
        axes[4,col].set_xlabel('Simulation minute')
        for ax in axes[:,col]:ax.set_xlim(177,188);ax.grid(axis='y',alpha=.2)
    for i,label in enumerate(('Actual SPOT quote mid\n(tick)','Maker 4 center / reference\n(tick)','Actual inventory\n(lots)','Private contribution\nbefore clamp (tick)','Flow contribution\nbefore clamp (tick)')):axes[i,0].set_ylabel(label)
    axes[1,0].legend();axes[2,0].legend()
    fig.suptitle('Residual-wave inventory hypothesis | seed 41 | actual observations',fontsize=15)
    fig.text(.5,.015,'Same selected minutes 177–188; paths and inventories diverge endogenously. Common scales per row; no smoothing.\nPrivate components include reference bias. Final clipping still applies. The pilot does not replace all four full runs.',ha='center')
    fig.tight_layout(rect=[0,.055,1,.96]);fig.savefig(root/'remaining-window-comparison.png',dpi=150);plt.close(fig)
    print(json.dumps([{k:v for k,v in r.items() if k not in ('source_hashes','largest_confirmed_reconsideration_jumps')} for r in result],indent=2))


if __name__=='__main__':summarize()
