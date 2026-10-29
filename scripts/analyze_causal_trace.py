"""Decompose actual maker/basis observations; preserve ordered execution evidence."""
from __future__ import annotations
import argparse, csv, json, math
from collections import Counter, defaultdict
from pathlib import Path
from analyze_market_mechanisms import sha, write

ANALYZER_SCHEMA=3


def account(observation):
    value=observation.get('account')
    if not value:return {}
    return next(iter(value.values()))


def top_mid(book):
    if book['bids'] and book['asks']:
        return (book['bids'][0]['price_tick']+book['asks'][0]['price_tick'])/2


def clip(x, reach):return min(reach,max(-reach,x))


def order_details(action):
    if isinstance(action,dict):
        for key in ('PlacePostOnly','PlaceProtected','PlaceImmediateOrCancel'):
            if key in action:return action[key]
    return None


def analyze(trace, recipe, out):
    population=json.loads(recipe.read_text(encoding='utf-8'))
    configs={p['Plugin']['participant']['participant_id']:p['Plugin'] for p in population['agents']}
    role={(p['participant']['account_id'],p['participant']['instrument_id']):p['plugin_id'] for p in configs.values()}
    maker=[];basis=[];executions=[];counts=Counter();flow=Counter();gateway=[]
    with trace.open(encoding='utf-8') as f:
        for line in f:
            row=json.loads(line);kind=row['kind'];counts[kind]+=1
            if kind=='decision':
                p=configs[row['participant']];c=p['config'];v=row['observation'];a=account(v);before,after=row['before'],row['after']
                common=dict(time_ms=v['time_ms'],participant=row['participant'],market=v['instrument'],position=int(a['position_qty']),book_mid=top_mid(v['book']),
                    actions=len(row['actions']),cancel_actions=sum(isinstance(x,dict) and 'Cancel' in x for x in row['actions']),
                    quoted_qty=sum(o['qty'] for x in row['actions'] if (o:=order_details(x)) is not None),
                    cash=int(a.get('available_cash',a.get('equity',0))),reserved=int(a.get('reserved_cash',a.get('reserved_margin',0))),
                    buy_open=sum(o['remaining_qty'] for o in v['orders'] if o['side']=='Buy'),sell_open=sum(o['remaining_qty'] for o in v['orders'] if o['side']=='Sell'))
                if p['plugin_id']=='AdaptiveLiquidityMaker':
                    mid=after['last_mid'];gamma=c['gamma_ppm']/1e6;sig=after['sigma'];risk=after['k'];intensity=after['a']
                    if c['model']=='GLFT':
                        c2=math.sqrt(gamma/(2*intensity*risk)*(1+gamma/risk)**(risk/gamma+1))
                        skew=c2*sig*c['skew_ppm']/1e6*c['inventory_cap']
                    else:skew=gamma*sig
                    q=(common['position']-c['inventory_target'])/c['inventory_cap']
                    eb,ea=v['external_bid'],v['external_ask'];micro=mid
                    if eb and ea:micro=(eb['price_tick']*ea['qty']+ea['price_tick']*eb['qty'])/(eb['qty']+ea['qty'])
                    reference=mid+(micro-mid)*c['microprice_ppm']/1e6
                    index=(v.get('perp_price') or {}).get('index_price_tick')
                    if index is not None:reference+=(index-reference)*c['index_weight_ppm']/1e6
                    raw=reference-mid-q*skew;reach=c['max_half_spread_ticks'];bounded=clip(raw,reach)
                    # Observation-based confirmation can advance memory before
                    # center calculation; this is still before gateway feedback.
                    memory=after.get('last_reservation_offset',0)
                    fresh=[t for t in v['public_trades'] if before.get('last_trade_id') is None or t['trade_id']>before['last_trade_id']]
                    signed=before.get('pending_signed',0)+sum(t['qty']*(1 if t['taker_side']=='Buy' else -1) for t in fresh)
                    total=before.get('pending_total',0)+sum(t['qty'] for t in fresh)
                    if c.get('market_depth_flow'):
                        visible=v['external_ask_levels'] if signed>=0 else v['external_bid_levels']
                        depth=sum(l['qty'] for l in visible if abs(l['price_tick']-mid)<=reach)
                    else:depth=c['max_qty']*c['levels']
                    impact=(signed/(total+depth) if total+depth>0 else 0)*after['half_spread']*c['flow_skew_ppm']/1e6
                    preflow=bounded-memory if c.get('reservation_innovations',False) else raw
                    expected=mid+clip(preflow+impact,reach)
                    # A cancel-only pass may reset next_ms to the same due time.
                    # Comparing the two deadlines would miss that recomputation.
                    reconsidered=(v['time_ms']>=before['next_ms'] and before.get('last_ms')!=v['time_ms'] and after.get('last_ms')==v['time_ms'])
                    maker.append(dict(**common,reconsidered=reconsidered,mid=mid,inventory_error=common['position']-c['inventory_target'],sigma=sig,k=risk,intensity=intensity,
                        center=after['center'],half_spread=after['half_spread'],reference_bias=reference-mid,inventory_bias=-q*skew,reservation_offset=bounded,previous_offset=memory,
                        flow_impact=impact,expected_center=expected,reconstruction_error=after['center']-expected if reconsidered else None,
                        pending_flow_signed=signed,pending_flow_total=total,flow_reference_depth=depth,confirmed_offset=after.get('last_reservation_offset'),deadline_before_ms=before['next_ms'],next_ms=after['next_ms']))
                else:
                    peer=v['related'][0];spot,perp=(peer,v) if c['leg']=='Perp' else (v,peer)
                    s,p0=account(spot),account(perp);sp,pp=int(s['position_qty']),int(p0['position_qty']);sm,pm=top_mid(spot['book']),top_mid(perp['book'])
                    rich=cheap=None
                    if all(x['book']['bids'] and x['book']['asks'] for x in (spot,perp)):
                        rich=(perp['book']['bids'][0]['price_tick']/spot['book']['asks'][0]['price_tick']-1)*1e6
                        cheap=(perp['book']['asks'][0]['price_tick']/spot['book']['bids'][0]['price_tick']-1)*1e6
                    orders=[o for x in row['actions'] if (o:=order_details(x)) is not None]
                    lead_entry=bool(c['leg']=='Perp' and orders and (pp==0 or ((pp>0)==(orders[0]['side']=='Buy'))))
                    executable_entry=None
                    if lead_entry and rich is not None:
                        executable_entry=cheap < -c['entry_ppm'] if orders[0]['side']=='Buy' else rich > c['entry_ppm']
                    residual=sp+pp-c['neutral_spot'];hedge_buy=residual<0
                    hedge_top=(spot['book']['asks'] if hedge_buy else spot['book']['bids'])
                    legacy_limit=sm*(1+(1 if hedge_buy else -1)*c['max_slippage_ppm']/1e6) if sm else None
                    hedge_limit=hedge_top[0]['price_tick'] if p['plugin_version']=='1.2.0' and hedge_top else legacy_limit
                    hedge_reachable=bool(hedge_top and hedge_limit is not None and ((hedge_top[0]['price_tick']<=hedge_limit) if hedge_buy else (hedge_top[0]['price_tick']>=hedge_limit)))
                    legacy_reachable=bool(hedge_top and legacy_limit is not None and ((hedge_top[0]['price_tick']<=legacy_limit) if hedge_buy else (hedge_top[0]['price_tick']>=legacy_limit)))
                    basis.append(dict(**common,leg=c['leg'],spot_position=sp,perp_position=pp,residual=residual,mid_basis_ppm=(pm/sm-1)*1e6 if sm and pm else None,
                        rich_executable_ppm=rich,cheap_executable_ppm=cheap,lead_entry=lead_entry,executable_entry=executable_entry,
                        spot_available_cash=int(s['available_cash']),spot_available_position=int(s['available_position']),
                        spot_hedge_reachable=hedge_reachable,legacy_midpoint_band_reachable=legacy_reachable,spot_hedge_limit=hedge_limit,spot_hedge_top_price=hedge_top[0]['price_tick'] if hedge_top else None,
                        entry_count=after['entries'],unwind_count=after['unwinds'],hedge_count=after['hedges']))
            elif kind=='gateway':
                e=row['execution'];events=[x['event'] for x in e['result'].get('events',[])]
                gateway.append(dict(time_ms=e['time_ms'],participant=row['participant'],command_seq=e['command_seq'],market=e['instrument'],action=row['action'],
                    before=row['before']['account'],after=row['after']['account'],book_before=row['before']['book'],book_after=row['after']['book'],accepted=e['result']['accepted'],
                    rested=sum(isinstance(x,dict) and 'OrderRested' in x for x in events),
                    rejected_events=[x for x in events if isinstance(x,dict) and any(k in x for k in ('OrderRejected','RiskRejected','CancelRejected'))]))
            elif kind=='authoritative':
                e=row['execution']
                if not e['result']['accepted']:continue
                for wrapped in e['result']['events']:
                    event=wrapped['event']
                    if isinstance(event,dict) and 'TradePrinted' in event:
                        t=event['TradePrinted'];r=role.get((t['taker_account_id'],e['instrument']),'external')
                        flow[(r,'buy' if t['taker_side']=='Buy' else 'sell')]+=t['qty']
                        executions.append(dict(time_ms=e['time_ms'],market=e['instrument'],command_seq=e['command_seq'],liquidation=e['liquidation'],taker_role=r,**t))
    out.mkdir(parents=True,exist_ok=True)
    for name,rows in [('maker-decisions',maker),('basis-decisions',basis),('trades',executions)]:
        if rows:
            with (out/(name+'.csv')).open('w',newline='',encoding='utf-8') as f:
                w=csv.DictWriter(f,fieldnames=list(rows[0]));w.writeheader();w.writerows(rows)
    entries=[r for r in basis if r['lead_entry']];false_entries=[r for r in entries if r['executable_entry'] is False]
    errors=[abs(r['reconstruction_error']) for r in maker if r['reconstruction_error'] is not None]
    summary=dict(analyzer_schema=ANALYZER_SCHEMA,analyzer_sha256=sha(Path(__file__)),trace_sha256=sha(trace),recipe_sha256=sha(recipe),counts=dict(counts),maker_rows=len(maker),basis_rows=len(basis),trades=len(executions),
        lead_entries=len(entries),unexecutable_at_configured_threshold=len(false_entries),unexecutable_entry_qty=sum(r['quoted_qty'] for r in false_entries),
        liquidation_trades=sum(r['liquidation'] for r in executions),maker_center_max_reconstruction_error=max(errors,default=None),
        flow_by_role={f'{a}:{b}':v for (a,b),v in flow.items()},
        scope='Retrospective selected window. Maker components reconstruct the existing equations at tick size 1; center errors expose unsupported/missed branches instead of being silently ignored. Quote and inventory observations precede the action wave; gateway snapshots reflect ordered actual submissions. Authoritative executions identify liquidation explicitly. Entry classification is an increase in same-direction perp exposure, not a profit attribution.')
    write(out/'trace-summary.json',summary);write(out/'gateway-accounts.json',gateway)
    print(json.dumps(summary),flush=True)
    return summary


if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('trace',type=Path);p.add_argument('recipe',type=Path);p.add_argument('output',type=Path)
    a=p.parse_args();analyze(a.trace,a.recipe,a.output)
