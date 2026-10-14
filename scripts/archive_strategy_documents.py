#!/usr/bin/env python3
"""Preserve primary strategy/model documents and earlier TradingView captures."""
from datetime import datetime, timezone
from pathlib import Path
import hashlib
import json
import os
import shutil
import ssl
import subprocess
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
LIB = ROOT/'.local/strategy-library-20261010'
PAPERS = [
    ('avellaneda-stoikov', 'High-frequency trading in a limit order book', 'https://people.orie.cornell.edu/sfs33/LimitOrderBook.pdf', None),
    ('glft', 'Dealing with the Inventory Risk', 'https://arxiv.org/pdf/1105.3115', 'https://arxiv.org/abs/1105.3115'),
    ('queue-reactive', 'Simulating and analyzing order book data: The queue-reactive model', 'https://arxiv.org/pdf/1312.0563', 'https://arxiv.org/abs/1312.0563'),
    ('heterogeneous-rules', 'The Impact of Heterogeneous Trading Rules on the Limit Order Book and Order Flows', 'https://arxiv.org/pdf/0711.3581', 'https://arxiv.org/abs/0711.3581'),
    ('abides', 'ABIDES: Towards High-Fidelity Market Simulation for AI Research', 'https://arxiv.org/pdf/1904.12066', 'https://arxiv.org/abs/1904.12066'),
    ('bse', 'BSE: A Minimal Simulation of a Limit-Order-Book Stock Exchange', 'https://arxiv.org/pdf/1809.06027', 'https://arxiv.org/abs/1809.06027'),
    ('perpetual-futures', 'Fundamentals of Perpetual Futures', 'https://arxiv.org/pdf/2212.06888', 'https://arxiv.org/abs/2212.06888'),
    ('directional-market-making', 'High-frequency market-making with inventory constraints and directional bets', 'https://arxiv.org/pdf/1206.4810', 'https://arxiv.org/abs/1206.4810'),
    ('market-making-lecture', 'Market Making lecture', 'https://web.stanford.edu/~ashlearn/RLForFinanceBook/MarketMaking.pdf', None),
    ('optimal-execution-lecture', 'Optimal Trade Execution lecture', 'https://web.stanford.edu/~ashlearn/RLForFinanceBook/chapter9.pdf', None),
]
TV = {
    'squeeze': 'https://www.tradingview.com/script/nqQ1DT5a-Squeeze-Momentum-Indicator-LazyBear/',
    'supertrend': 'https://www.tradingview.com/script/r6dAP7yi/',
    'wavetrend': 'https://www.tradingview.com/script/2KE8wTuF-Indicator-WaveTrend-Oscillator-WT/',
    'ut_alerts': 'https://www.tradingview.com/script/n8ss8BID-UT-Bot-Alerts/',
    'chandelier': 'https://www.tradingview.com/script/AqXxNS7j-Chandelier-Exit-everget/',
    'ut_strategy': 'https://www.tradingview.com/script/VKMgZdYr-UT-Bot-Strategy/',
    'ssl': 'https://www.tradingview.com/script/xzIoaIJC-SSL-channel/',
}


def fetch(url, path, pdf=False):
    row = dict(url=url,local_path=str(path),fetched_at=datetime.now(timezone.utc).isoformat())
    try:
        req=urllib.request.Request(url,headers={'User-Agent':'MarketForge-StrategyResearch/1.0'})
        try:
            with urllib.request.urlopen(req,timeout=35) as response:
                data=response.read(48*1024*1024+1)
                row.update(final_url=response.url,content_type=response.headers.get('Content-Type'))
        except urllib.error.URLError as exc:
            if os.name!='nt' or not isinstance(exc.reason,ssl.SSLCertVerificationError):
                raise
            path.parent.mkdir(parents=True,exist_ok=True)
            # Native Windows certificate validation can use the host's trust
            # configuration. Do not disable TLS validation for document capture.
            def ps_literal(value):
                return "'"+str(value).replace("'","''")+"'"
            command="$ErrorActionPreference='Stop'; Invoke-WebRequest -UseBasicParsing -Uri "+ps_literal(url)+" -OutFile "+ps_literal(path)+" -TimeoutSec 35"
            subprocess.run(['powershell.exe','-NoProfile','-NonInteractive','-Command',command],capture_output=True,check=True,timeout=45)
            data=path.read_bytes()
            row.update(fetch_method='PowerShell Invoke-WebRequest with platform certificate validation',previous_attempt_error=str(exc))
        if len(data)>48*1024*1024:
            raise ValueError('document exceeds collection size limit')
        if pdf and not data.startswith(b'%PDF-'):
            raise ValueError('response is not a PDF')
        path.parent.mkdir(parents=True,exist_ok=True)
        path.write_bytes(data)
        row.update(status='saved',size_bytes=len(data),sha256=hashlib.sha256(data).hexdigest())
    except Exception as exc:
        row.update(status='failed',error=str(exc))
    return row


def main():
    rows=[]
    LIB.mkdir(parents=True,exist_ok=True)
    for sid,title,pdf,abstract in PAPERS:
        row=dict(id=sid,title=title,kind='primary_paper_or_author_lecture',license_review='document_specific_pending',full_paper_review=False)
        row['pdf']=fetch(pdf,LIB/'papers'/(sid+'.pdf'),True)
        if abstract:
            row['abstract']=fetch(abstract,LIB/'papers'/(sid+'.html'))
        rows.append(row)
        print(sid,row['pdf']['status'],flush=True)
        (LIB/'documents.json').write_text(json.dumps(rows,ensure_ascii=False,indent=2),encoding='utf-8')
    tv=[]
    for sid,url in TV.items():
        src=ROOT/'.local/strategy-research-20261010/originals'/(sid+'.pine')
        dst=LIB/'tradingview'/(sid+'.pine')
        dst.parent.mkdir(parents=True,exist_ok=True)
        if src.exists():
            shutil.copyfile(src,dst)
            tv.append(dict(id=sid,url=url,local_path=str(dst),sha256=hashlib.sha256(dst.read_bytes()).hexdigest(),
                capture_date='2026-10-10',capture_method='visible_source_text_nbsp_normalized',upstream_byte_exact=False,
                execution_evidence=str(ROOT/'docs/validation/2026-10-10-strategy-research-results.json'),license_review='per_script_pending'))
    (LIB/'tradingview.json').write_text(json.dumps(tv,ensure_ascii=False,indent=2),encoding='utf-8')
    # Keep earlier controlled probes and their receipts together with the library.
    earlier=ROOT/'.local/strategy-research-20261010'
    for relative in ['run','boundaries.json']:
        src=earlier/relative; dst=LIB/'previous-probes'/relative
        if src.is_dir():
            shutil.copytree(src,dst,dirs_exist_ok=True)
        elif src.is_file():
            dst.parent.mkdir(parents=True,exist_ok=True); shutil.copyfile(src,dst)
    print('documents',sum(x['pdf']['status']=='saved' for x in rows),'tradingview',len(tv),flush=True)


if __name__=='__main__':
    main()
