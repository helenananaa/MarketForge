#!/usr/bin/env python3
"""Archive public strategy sources for research without installing or running them.

Creates independent source ZIPs, pinned commits, content hashes, a static triage
inventory, notebook/code-fence extracts and an offline HTML file index.
"""
from __future__ import annotations

import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import hashlib
import html
import json
import os
from pathlib import Path
import re
import subprocess
from urllib.parse import quote
import zipfile

ROOT = Path(__file__).resolve().parents[1]

# Sparse selections archive strategy-bearing paths instead of market datasets.
SOURCES = [
    ('everget', 'everget/tradingview-pinescript-indicators', 'pine_indicators', [], 'strategy-research-20261010/everget'),
    ('jamesbachini', 'jamesbachini/Pine-Script-Examples', 'pine_strategies', [], 'strategy-research-20261010/jamesbachini'),
    ('freqtrade-strategies', 'freqtrade/freqtrade-strategies', 'python_strategies', [], 'strategy-research-20261010/freqtrade-strategies'),
    ('nostalgia', 'iterativv/NostalgiaForInfinity', 'python_composite', [], 'strategy-research-20261010/NostalgiaForInfinity'),
    ('hasnocool', 'hasnocool/tradingview-pine-scripts', 'pine_strategies', [], None),
    ('f13end', 'f13end/tradingview-custom-indicators', 'pine_indicators_strategies', [], None),
    ('dearvn-futures', 'dearvn/trading-futures-tradingview-script', 'pine_futures', [], None),
    ('dearvn-binance', 'dearvn/tradingview-pinscript-futures-binance', 'pine_perpetual', [], None),
    ('pine-lepoto', 'LePotoRico2105/pine-script-strategies', 'pine_strategies', [], None),
    ('pine-ahmed', 'Ahmed-GoCode/tradingview-strategy-collection', 'pine_strategies', [], None),
    ('fmz', 'fmzquant/strategies', 'multilanguage_strategies', [], None),
    ('hummingbot', 'hummingbot/hummingbot', 'market_making_arbitrage', ['controllers','scripts','hummingbot/strategy','hummingbot/strategy_v2'], None),
    ('hummingbot-docs', 'hummingbot/hummingbot-site', 'decision_docs', ['docs/strategies','docs/blog'], None),
    ('hftbacktest', 'nkaz001/hftbacktest', 'microstructure_python_rust', ['examples','hftbacktest/examples','py-hftbacktest/examples','docs'], None),
    ('nautilus', 'nautechsystems/nautilus_trader', 'python_rust_strategies', ['examples','nautilus_trader/examples','crates/execution','crates/trading/src/examples','crates/backtest/examples','crates/adapters/dydx/examples','crates/adapters/kraken/examples','docs/concepts','docs/tutorials'], None),
    ('lean', 'QuantConnect/Lean', 'python_csharp_strategies', ['Algorithm.Python','Algorithm.CSharp','Algorithm.Framework'], None),
    ('vnpy-cta', 'vnpy/vnpy_ctastrategy', 'python_cta', [], None),
    ('vnpy-algo', 'vnpy/vnpy_algotrading', 'execution_algorithms', [], None),
    ('vnpy-spread', 'vnpy/vnpy_spreadtrading', 'spread_arbitrage', [], None),
    ('jesse-examples', 'jesse-ai/example-strategies', 'python_strategies', [], None),
    ('backtrader', 'mementum/backtrader', 'python_examples', ['samples','backtrader/strategies'], None),
    ('backtestingpy', 'kernc/backtesting.py', 'python_examples', ['examples','doc'], None),
    ('vectorbt', 'polakowo/vectorbt', 'python_notebooks', ['examples','docs'], None),
    ('abides', 'abides-sim/abides', 'agent_behaviors', ['agent','config','util'], None),
    ('abides-jpmc', 'jpmorganchase/abides-jpmc-public', 'agent_behaviors', ['abides-markets/abides_markets/agents','abides-markets/abides_markets/configs','abides-gym/abides_gym/envs'], None),
    ('bse', 'davecliff/BristolStockExchange', 'adaptive_auction_agents', [], None),
    ('avellaneda', 'fedecaccia/avellaneda-stoikov', 'optimal_market_making', [], None),
    ('finrl', 'AI4Finance-Foundation/FinRL', 'reinforcement_learning', ['examples','finrl/agents','finrl/meta/env_stock_trading','finrl/meta/env_portfolio_allocation','docs'], None),
    ('tensortrade', 'tensortrade-org/tensortrade', 'reinforcement_learning', ['examples','tensortrade/agents','tensortrade/env','docs'], None),
    ('qlib', 'microsoft/qlib', 'learned_decision_models', ['examples/benchmarks','examples/rl','qlib/rl','docs'], None),
    ('cpp-mm', 'abhishektang/RL-HFT-OrderBook', 'cpp_market_making', [], None),
    ('almgren-chriss', 'sauloduttra/almgren-chriss', 'optimal_execution', [], None),
    ('ac-notebook', 'joshuapjacob/almgren-chriss-optimal-execution', 'optimal_execution_notebook', [], None),
]

TEXT_EXT = {'.pine','.ps','.py','.pyx','.rs','.cpp','.cc','.c','.h','.hpp','.cs','.js','.ts','.ipynb','.md','.rst','.txt','.json','.yaml','.yml'}


def stamp():
    return datetime.now(timezone.utc).isoformat()


def git(args, cwd=None, timeout=240):
    env = dict(os.environ, GIT_TERMINAL_PROMPT='0', GIT_LFS_SKIP_SMUDGE='1')
    p = subprocess.run(['git', *args], cwd=cwd, env=env, capture_output=True, timeout=timeout)
    if p.returncode:
        raise RuntimeError(p.stderr.decode('utf-8', errors='replace')[-2500:])
    return p.stdout.decode('utf-8', errors='replace').strip()


def language(path, text=''):
    ext = path.suffix.lower()
    if ext in ('.md','.ipynb','.rst'):
        return {'.md':'markdown','.ipynb':'notebook','.rst':'documentation'}[ext]
    if ext in ('.pine','.ps') or ('//@version=' in text and re.search(r'\b(study|strategy|indicator)\s*\(', text)):
        return 'pine'
    return {'.py':'python','.pyx':'cython','.rs':'rust','.cs':'csharp','.cpp':'cpp','.cc':'cpp',
            '.c':'c','.js':'javascript','.ts':'typescript','.ipynb':'notebook','.md':'markdown','.rst':'documentation'}.get(ext, 'other')


def flags(text, path):
    result = []
    checks = {
        'lookahead_marker': r'lookahead\s*=\s*(?:barmerge\.)?lookahead_on|shift\s*\(\s*-\d',
        'named_bias_or_repaint': r'lookahead_bias|repaint',
        'external_context_marker': r'request\.(?:security|financial|economic)|\bsecurity\s*\(|informative_pairs|Add(?:Equity|Forex|Option|Future)|add_(?:equity|forex|option|future)',
        'multi_asset_marker': r'cointegration|triangular|cross.exchange|portfolio|pairs.trad',
        'trained_model_marker': r'PPO|DQN|SAC|RLlib|stable_baselines|torch|tensorflow|LightGBM|XGBoost',
        'execution_or_book_marker': r'order.?book|order.flow|imbalance|microprice|queue|iceberg|TWAP|VWAP|market.mak',
        'funding_marker': r'funding.rate|funding_rate|fundingRate',
        'risk_orders_marker': r'strategy\.exit|stop.loss|trailing|reduce.only|margin|liquidat',
    }
    for label, pattern in checks.items():
        if re.search(pattern, text+' '+path.as_posix(), re.I):
            result.append(label)
    return result


def extract_code(row, text, root):
    fragments = []
    lang = row['language']
    if lang == 'notebook':
        try:
            nb = json.loads(text)
            if nb.get('metadata', {}).get('language_info', {}).get('name', 'python') == 'python':
                code = '\n\n'.join(''.join(c.get('source', [])) for c in nb.get('cells', []) if c.get('cell_type')=='code')
                if code.strip():
                    fragments.append(('python',code,'notebook_cells'))
        except (ValueError, TypeError):
            pass
    elif lang == 'markdown':
        for match in re.finditer(r'^```([^\n]*)\n(.*?)^```\s*$', text, re.M|re.S):
            hint, code = match.group(1).strip().lower(), match.group(2)
            detected = language(Path('fragment.txt'), code)
            if detected == 'pine' or hint in ('pine','pinescript'):
                fragments.append(('pine',code,'markdown_fence'))
            elif hint in ('python','py','javascript','js','cpp','c++','rust','csharp','c#','mylanguage','pascal','blockly'):
                fragments.append(({'py':'python','js':'javascript','c++':'cpp','c#':'csharp'}.get(hint,hint),code,'markdown_fence'))
            elif re.search(r'function\s+main\s*\(|def\s+main\s*\(|void\s+main\s*\(',code):
                detected = 'python' if 'def main' in code else 'cpp' if '#include' in code else 'javascript'
                fragments.append((detected,code,'markdown_fence_inferred'))
    for i,(lang,code,kind) in enumerate(fragments):
        suffix = {'pine':'.pine','python':'.py','javascript':'.js','cpp':'.cpp','rust':'.rs','csharp':'.cs','mylanguage':'.mylang','pascal':'.pas','blockly':'.xml'}[lang]
        token = hashlib.sha256((row['path']+str(i)).encode()).hexdigest()[:12]
        rel = Path('extracts')/row['source_id']/(token+suffix)
        path = root/rel
        path.parent.mkdir(parents=True,exist_ok=True)
        # Use exact UTF-8 bytes: write_text would translate LF on Windows and
        # make recorded content hashes disagree with the preserved fragments.
        path.write_bytes(code.encode('utf-8'))
        (root/(str(rel)+'.json')).write_text(json.dumps({'source':row['local_path'],'source_url':row['source_url'],
            'sha256':hashlib.sha256(code.encode()).hexdigest(),'kind':kind,'fragment_index':i,
            'runnable_claim':False},indent=2),encoding='utf-8')
        yield dict(source_id=row['source_id'],path=rel.as_posix(),language=lang,kind=kind,
                   parent=row['path'],sha256=hashlib.sha256(code.encode()).hexdigest())


def archive_source(spec, root):
    sid, repo, category, sparse, reuse = spec
    row = dict(id=sid,repo=repo,url='https://github.com/'+repo,category=category,
               requested_paths=sparse,fetched_at=stamp(),execution_status='not_executed')
    dest = ROOT/'.local'/reuse if reuse else root/'repos'/sid
    try:
        if not (dest/'.git').exists():
            git(['-c','core.hooksPath='+str(root/'disabled-hooks'),'clone','--depth','1','--filter=blob:none',
                 '--no-checkout',row['url']+'.git',str(dest)])
        actual = git(['remote','get-url','origin'],dest)
        if actual.rstrip('/').removesuffix('.git').lower()!=row['url'].lower():
            raise ValueError('existing checkout remote does not match')
        commit = git(['rev-parse','HEAD'],dest)
        # NUL records avoid Git's quoted Unicode names and embedded newlines.
        tree = [x for x in git(['ls-tree','-r','-z','--name-only','HEAD'],dest).split('\0') if x]
        available = [p for p in sparse if any(x==p or x.startswith(p+'/') for x in tree)]
        local_names = {}
        if not reuse:
            if sparse:
                if not available:
                    raise ValueError('none of the requested strategy paths exist')
                git(['sparse-checkout','set','--cone',*available],dest)
            try:
                git(['checkout','--force','HEAD'],dest)
            except RuntimeError as exc:
                if 'invalid path' not in str(exc):
                    raise
                # Git archive retains original names; Windows copies use a map.
                raw = root/'original-name-archives'/(sid+'-'+commit[:12]+'.zip')
                raw.parent.mkdir(parents=True,exist_ok=True)
                # git archive on Windows also validates NTFS paths. Read blobs
                # without materializing the tree, then ZIP their original names.
                git(['fetch','--refetch','--no-filter','--depth=1','origin',commit],dest)
                entries = subprocess.run(['git','ls-tree','-r','-z','HEAD'],cwd=dest,capture_output=True,check=True).stdout.split(b'\0')
                blobs=[]
                for entry in entries:
                    if not entry:
                        continue
                    info,name=entry.split(b'\t',1)
                    mode,kind,oid=info.split()
                    if kind==b'blob' and mode!=b'120000':
                        blobs.append((oid,name.decode('utf-8')))
                batch=subprocess.run(['git','cat-file','--batch'],cwd=dest,input=b''.join(oid+b'\n' for oid,name in blobs),capture_output=True,check=True,timeout=240).stdout
                cursor=0
                with zipfile.ZipFile(raw,'w',zipfile.ZIP_DEFLATED) as original:
                    for oid,name in blobs:
                        end=batch.index(b'\n',cursor)
                        header=batch[cursor:end].split()
                        if len(header)!=3 or header[0]!=oid or header[1]!=b'blob':
                            raise ValueError('unexpected Git blob response')
                        size=int(header[2]); cursor=end+1
                        original.writestr(name,batch[cursor:cursor+size]); cursor+=size+1
                readable = root/'readable'/sid
                used = set()
                with zipfile.ZipFile(raw) as z:
                    for entry in z.infolist():
                        if entry.is_dir() or ((entry.external_attr >> 16) & 0o170000)==0o120000:
                            continue
                        parts = []
                        for part in Path(entry.filename).parts:
                            if part in ('.','..') or not part:
                                raise ValueError('unsafe archive path')
                            cleaned = re.sub(r'[<>:"\\|?*\x00-\x1f]','_',part).rstrip(' .')
                            if cleaned.split('.')[0].upper() in {'CON','PRN','AUX','NUL',*[f'COM{i}' for i in range(1,10)],*[f'LPT{i}' for i in range(1,10)]}:
                                cleaned = '_'+cleaned
                            parts.append(cleaned or '_')
                        rel = Path(*parts)
                        if str(rel).casefold() in used:
                            rel = rel.with_name(rel.stem+'-'+hashlib.sha256(entry.filename.encode()).hexdigest()[:8]+rel.suffix)
                        used.add(str(rel).casefold())
                        target = readable/rel
                        target.parent.mkdir(parents=True,exist_ok=True)
                        target.write_bytes(z.read(entry))
                        local_names[entry.filename]=str(target)
                (readable/'original-path-map.json').write_text(json.dumps(local_names,ensure_ascii=False,indent=2),encoding='utf-8')
                row.update(windows_path_fallback=True,checkout_error=str(exc),original_name_archive=str(raw))
        row.update(status='saved',commit=commit,local_path=str(dest),selected_paths=available,
                   missing_paths=sorted(set(sparse)-set(available)),reused=bool(reuse))
        files = []
        zip_path = root/'snapshots'/(sid+'-'+commit[:12]+'.zip')
        zip_path.parent.mkdir(parents=True,exist_ok=True)
        licenses = []
        with zipfile.ZipFile(zip_path,'w',zipfile.ZIP_DEFLATED) as z:
            for name in tree:
                path = Path(local_names[name]) if name in local_names else dest/name
                if not path.is_file() or path.is_symlink():
                    continue
                data = path.read_bytes()
                z.writestr(name,data)
                text = data.decode('utf-8',errors='replace') if path.suffix.lower() in TEXT_EXT or path.name.lower().startswith(('license','copying','notice','readme')) else ''
                is_license = path.name.lower().startswith(('license','copying','notice'))
                if is_license:
                    licenses.append(dict(path=name,sha256=hashlib.sha256(data).hexdigest(),excerpt=text[:500]))
                item = dict(source_id=sid,path=name,local_path=str(path),size_bytes=len(data),
                            sha256=hashlib.sha256(data).hexdigest(),language=language(path,text),
                            source_url=row['url']+'/blob/'+commit+'/'+quote(name,safe='/'),
                            signals=flags(text[:500000],Path(name)),license_review='per_file_pending',
                            execution_status='not_executed')
                files.append(item)
        row.update(snapshot=str(zip_path),snapshot_sha256=hashlib.sha256(zip_path.read_bytes()).hexdigest(),
                   license_files=licenses,license_status='license_files_preserved' if licenses else 'no_license_file_found',
                   file_count=len(files),bytes=sum(x['size_bytes'] for x in files),
                   languages=dict(Counter(x['language'] for x in files)))
        return row, files
    except Exception as e:
        row.update(status='failed',error=str(e))
        return row, []


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--root',type=Path,default=ROOT/'.local/strategy-library-20261010')
    p.add_argument('--only',nargs='*')
    p.add_argument('--extract-only',action='store_true',help='regenerate snippets from existing pinned files without refetching sources')
    args=p.parse_args()
    root=args.root.resolve()
    root.mkdir(parents=True,exist_ok=True)
    specs=[] if args.extract_only else [s for s in SOURCES if not args.only or s[0] in args.only]
    sources, inventory, extracts = [], [], []
    # Partial retries retain previously archived source metadata and inventories.
    if args.extract_only:
        sources=json.loads((root/'manifest.json').read_text(encoding='utf-8'))['sources']
        inventory=[json.loads(line) for line in (root/'inventory.jsonl').read_text(encoding='utf-8').splitlines()]
    elif args.only and (root/'manifest.json').exists():
        old=json.loads((root/'manifest.json').read_text(encoding='utf-8'))
        sources=[x for x in old['sources'] if x['id'] not in args.only]
        for name,target in [('inventory.jsonl',inventory),('extracts.jsonl',extracts)]:
            if (root/name).exists():
                target.extend(x for x in (json.loads(line) for line in (root/name).read_text(encoding='utf-8').splitlines()) if x['source_id'] not in args.only)
    with ThreadPoolExecutor(max_workers=3) as pool:
        jobs={pool.submit(archive_source,s,root):s[0] for s in specs}
        for job in as_completed(jobs):
            row, files=job.result()
            sources.append(row)
            inventory.extend(files)
            print(row['id'],row['status'],row.get('file_count'),row.get('error','')[:150],flush=True)
            (root/'sources-progress.json').write_text(json.dumps(sources,ensure_ascii=False,indent=2),encoding='utf-8')
    for row in inventory:
        if row['language'] in ('notebook','markdown') and (not args.only or row['source_id'] in args.only):
            try:
                text=Path(row['local_path']).read_text(encoding='utf-8',errors='replace')
                extracts.extend(extract_code(row,text,root))
            except OSError:
                pass
    manifest=dict(created_at=stamp(),root=str(root),sources=sorted(sources,key=lambda x:x['id']),
                  source_files=len(inventory),unique_contents=len({x['sha256'] for x in inventory}),
                  extract_fragments=len(extracts),language_files=dict(Counter(x['language'] for x in inventory)),
                  limitations=['File counts include infrastructure, documentation and utilities; not strategy counts',
                               'Static marker flags are triage hints, not confirmed defects',
                               'Preserved code was not installed or executed; license and data adaptation pending'])
    (root/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2),encoding='utf-8')
    for name,rows in [('inventory.jsonl',inventory),('extracts.jsonl',extracts)]:
        with (root/name).open('w',encoding='utf-8') as out:
            for row in sorted(rows,key=lambda x:(x['source_id'],x['path'])):
                out.write(json.dumps(row,ensure_ascii=False)+'\n')
    body=['<!doctype html><meta charset="utf-8"><title>Strategy source library</title>',
          '<style>body{font:15px system-ui;margin:24px;max-width:1400px}input{padding:10px;width:95%}td{padding:5px;border-bottom:1px solid #ddd}small{color:#666}table{width:100%}</style>',
          '<h1>本地策略来源库</h1><p>原始文件与快照；尚未安装或执行。文件数不等于独立策略数。</p>',
          '<input id="q" placeholder="筛选名称、来源、语言、静态标记"><p id="count"></p><table id="files">']
    index_rows=inventory+[dict(x,local_path=str(root/x['path']),signals=[x['kind'],'fragment_not_execution_verified']) for x in extracts]
    for row in sorted(index_rows,key=lambda x:(x['source_id'],x['path'])):
        if row['language']=='other':
            continue
        body.append('<tr><td>'+html.escape(row['source_id'])+'</td><td><a href="'+html.escape(Path(row['local_path']).as_uri())+'">'+html.escape(row['path'])+'</a></td><td>'+row['language']+'</td><td><small>'+html.escape(', '.join(row['signals']))+'</small></td></tr>')
    body.append('</table><script>const rows=[...document.querySelectorAll("tr")];function filter(){let q=document.querySelector("#q").value.toLowerCase(),n=0;for(let r of rows){let ok=r.textContent.toLowerCase().includes(q);r.hidden=!ok;n+=ok}document.querySelector("#count").textContent=n+" 个文件"}document.querySelector("#q").oninput=filter;filter()</script>')
    (root/'index.html').write_text('\n'.join(body),encoding='utf-8')
    print(json.dumps({k:v for k,v in manifest.items() if k!='sources'},ensure_ascii=False),flush=True)


if __name__=='__main__':
    main()
