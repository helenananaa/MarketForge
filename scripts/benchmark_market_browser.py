"""Capture real MarketForge page frames while isolated Release bot rooms trade.

Requires a built web bundle served at --web-url and the Playwright CLI browser.
Artifacts contain snapshots, screenshots, frame gaps, long tasks and API timing.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
PROBE = r'''async page => {
  let failedRequests=0,httpErrors=0;
  const failed=request=>{if(request.url().includes('/rooms/'))failedRequests++;};
  const response=result=>{if(result.url().includes('/rooms/')&&!result.ok())httpErrors++;};
  page.on('requestfailed',failed);page.on('response',response);
  const result=await page.evaluate(async () => {
    const frames=[],tasks=[]; let previous=null,done=false,mutations=0;
    const start=performance.now(),bodyBefore=document.body.innerText;
    const observer=new PerformanceObserver(list => {
      for(const entry of list.getEntries()) tasks.push(entry.duration);
    });
    observer.observe({type:'longtask',buffered:false});
    const changes=new MutationObserver(list => { mutations+=list.length; });
    changes.observe(document.querySelector('main'),{subtree:true,childList:true,characterData:true});
    const tick=now => { if(done)return; if(previous!==null)frames.push(now-previous);previous=now;requestAnimationFrame(tick); };
    requestAnimationFrame(tick);
    await new Promise(resolve=>setTimeout(resolve,10000));done=true;observer.disconnect();changes.disconnect();
    const resources=performance.getEntriesByType('resource').filter(e=>e.startTime>=start && e.name.includes('/rooms/'))
      .map(e=>({url:e.name,duration_ms:e.duration}));
    const q=(data,p)=>{const sorted=[...data].sort((a,b)=>a-b);return sorted.length?sorted[Math.ceil(sorted.length*p)-1]:null;};
    return {visibility:document.visibilityState,viewport:{width:innerWidth,height:innerHeight},
      duration_ms:performance.now()-start,frame_count:frames.length,frame_gap_p95_ms:q(frames,.95),
      frame_gap_max_ms:q(frames,1),frame_gaps_over_50ms:frames.filter(n=>n>50).length,
      long_tasks:tasks.length,long_task_max_ms:q(tasks,1),dom_mutations:mutations,
      body_changed:bodyBefore!==document.body.innerText,room_resources:resources,
      room_resource_p95_ms:q(resources.map(r=>r.duration_ms),.95),
      loaded_market:document.body.innerText.includes('V-BTC-SPOT')&&!document.body.innerText.includes('未载入房间')};
  });
  page.off('requestfailed',failed);page.off('response',response);
  return {...result,failed_room_requests:failedRequests,http_errors:httpErrors};
}'''

SLOW_PROBE = r'''async page => {
  const active=new Set();let maximum=0,completed=0,failed=0;
  const matches=request=>request.method()==='GET'&&/\/rooms\/.*\/(?:view|agents|events)(?:\?|$)/.test(request.url());
  const begun=request=>{if(matches(request)){active.add(request);maximum=Math.max(maximum,active.size);}};
  const finished=request=>{if(active.delete(request))completed++;};
  const rejected=request=>{if(active.delete(request))failed++;};
  const delay=async route=>{if(matches(route.request()))await new Promise(resolve=>setTimeout(resolve,1200));await route.continue();};
  page.on('request',begun);page.on('requestfinished',finished);page.on('requestfailed',rejected);
  await page.route('**/rooms/**',delay);
  await new Promise(resolve=>setTimeout(resolve,14000));
  await page.unrouteAll({behavior:'wait'});
  await new Promise(resolve=>setTimeout(resolve,1800));
  page.off('request',begun);page.off('requestfinished',finished);page.off('requestfailed',rejected);
  return {artificial_delay_ms:1200,maximum_active_refresh_requests:maximum,completed,failed,
    qualified:maximum<=3&&completed>=9&&failed===0};
}'''


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--recipe-template',type=Path,required=True)
    parser.add_argument('--counts',type=int,nargs='+',default=[38,100,200,500])
    parser.add_argument('--web-url',default='http://127.0.0.1:15281/')
    parser.add_argument('--session',default='market-load')
    parser.add_argument('--headed',action='store_true',help='open a visible Chrome window for desktop frame and layout validation')
    parser.add_argument('--slow-refresh',action='store_true',help='validate the last tier with 1200ms artificial read delay after the normal frame sample')
    args=parser.parse_args()
    args.output=args.output.resolve()
    if args.output.exists() and any(args.output.iterdir()):parser.error('use a fresh output directory to avoid stale connection receipts')
    args.output.mkdir(parents=True,exist_ok=True)
    probe=args.output/'probe.js';probe.write_text(PROBE,encoding='utf-8')
    slow_probe=args.output/'slow-probe.js';slow_probe.write_text(SLOW_PROBE,encoding='utf-8')
    npx=shutil.which('npx')
    if not npx:parser.error('npx is required for the Playwright CLI')
    flags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0
    def cli(directory,label,*command):
        result=subprocess.run([npx,'--yes','--package','@playwright/cli','playwright-cli',f'-s={args.session}',*command],
                              capture_output=True,text=True,encoding='utf-8',timeout=35,creationflags=flags)
        (directory/f'{label}.log').write_text(result.stdout+'\n'+result.stderr,encoding='utf-8')
        if result.returncode or '### Error' in result.stdout:raise RuntimeError(f'Playwright {label} failed')
        return result.stdout
    reports=[]
    for count in args.counts:
        directory=args.output/f'bots-{count}';directory.mkdir(exist_ok=True)
        with (directory/'load-run.log').open('w',encoding='utf-8') as log:
            process=subprocess.Popen([sys.executable,str(ROOT/'scripts/benchmark_market_load.py'),
                '--server',str(args.server.resolve()),'--output',str(directory/'service'),'--counts',str(count),
                '--seconds','5','--warmup','1','--browser-wait',str(65 if args.slow_refresh and count==args.counts[-1] else 40),
                '--recipe-template',str(args.recipe_template.resolve())],
                stdout=log,stderr=subprocess.STDOUT,creationflags=flags)
            try:
                connection_path=directory/f'service/bots-{count}/connection.json'
                deadline=time.perf_counter()+25
                while not connection_path.is_file():
                    if process.poll() is not None or time.perf_counter()>deadline:raise RuntimeError('service startup failed')
                    time.sleep(.1)
                connection=json.loads(connection_path.read_text(encoding='utf-8'))
                cli(directory,'open','open',args.web_url,*(['--headed'] if args.headed else []))
                snapshot=cli(directory,'before','snapshot')
                refs={label:re.search(rf'{re.escape(label)}.*?\[ref=([^\]]+)\]',snapshot).group(1)
                      for label in ('textbox "API base URL"','textbox "room id"','button "载入"')}
                cli(directory,'api','fill',refs['textbox "API base URL"'],connection['url'])
                cli(directory,'room','fill',refs['textbox "room id"'],connection['room'])
                cli(directory,'load','click',refs['button "载入"'])
                cli(directory,'loaded','snapshot')
                raw=cli(directory,'frames','run-code','--filename',str(probe))
                result=json.loads(raw.split('### Result\n',1)[1].split('\n###',1)[0])
                result.update(bots=count,connection=connection,web_url=args.web_url,browser_mode='headed' if args.headed else 'headless')
                if args.slow_refresh and count==args.counts[-1]:
                    delayed=cli(directory,'slow-refresh','run-code','--filename',str(slow_probe))
                    result['slow_refresh']=json.loads(delayed.split('### Result\n',1)[1].split('\n###',1)[0])
                cli(directory,'after','snapshot')
                # CLI saves the actual rendered page to its documented artifact path.
                cli(directory,'screenshot','screenshot','--filename',str(directory/'page.png'))
                (directory/'report.json').write_text(json.dumps(result,indent=2),encoding='utf-8')
                reports.append(result)
                print(json.dumps({k:result[k] for k in ('bots','loaded_market','frame_gap_p95_ms','frame_gap_max_ms','long_tasks','room_resource_p95_ms','dom_mutations')}),flush=True)
            finally:
                # The bounded service runner owns its child and always stops it.
                # Let it finish normally so all final API/memory receipts are saved.
                process.wait(timeout=65)
                if process.returncode:raise RuntimeError('service runner failed; see load-run.log')
    (args.output/'report.json').write_text(json.dumps({'cases':reports,'qualification':'10s visible-page samples; synthetic in-memory journal rooms, no production capacity guarantee'},indent=2),encoding='utf-8')


if __name__=='__main__':main()
