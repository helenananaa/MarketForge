"""Compare numerical analysis against a small full-tree control, without a market run."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
from unittest.mock import patch
from research_resources import windows_memory
from research_market_mechanisms import sha, write


def worker(directory, mode):
    from analyze_market_mechanisms import analyze_run
    row=json.loads((directory/'row.json').read_text(encoding='utf-8'))
    if mode=='tree':
        if (directory/'short.receipt.json').stat().st_size>64*1024**2:
            raise ValueError('Full-tree control is restricted to short receipts below 64 MiB')
        with patch('analyze_market_mechanisms.Receipt',side_effect=lambda path:json.loads(path.read_text(encoding='utf-8'))):
            result=analyze_run(directory,row)
    else:result=analyze_run(directory,row)
    write(directory/(mode+'.json'),result)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--directory',type=Path,required=True)
    parser.add_argument('--receipt',type=Path);parser.add_argument('--recipe',type=Path)
    parser.add_argument('--steps',type=int,default=1800)
    parser.add_argument('--worker',choices=['tree','stream'])
    args=parser.parse_args();directory=args.directory.resolve()
    if args.worker:return worker(directory,args.worker)
    if not 1<=args.steps<=3600:raise ValueError('Only short saved receipts are accepted')
    directory.mkdir(exist_ok=False)
    shutil.copyfile(args.receipt,directory/'short.receipt.json');shutil.copyfile(args.recipe,directory/'recipe.json')
    recipe=json.loads((directory/'recipe.json').read_text(encoding='utf-8'))
    write(directory/'row.json',dict(name='short',recipe=str(directory/'recipe.json'),recipe_sha256=sha(directory/'recipe.json'),
        steps=args.steps,seconds=1,bots=len(recipe['agents']),warmup_ms=1000,seed=41,profile='memory-validation'))
    write(directory/'short.run.json',dict(complete=True,receipt_sha256=sha(directory/'short.receipt.json'),
        inputs=dict(binary_sha256='same-receipt-analysis-control'),wall_seconds=1))
    results=[]
    for mode in ['tree','stream']:
        start=time.monotonic();samples=[]
        # The measured pre-fix runtime selected 24 OpenBLAS workers. Keep that
        # short control, versus the report's new one-worker default.
        env=dict(os.environ,OPENBLAS_NUM_THREADS='24'if mode=='tree'else'1',OMP_NUM_THREADS='1',MKL_NUM_THREADS='1')
        with (directory/(mode+'.log')).open('wb')as stream:
            child=subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'--directory',str(directory),'--worker',mode],env=env,stdout=stream,stderr=stream)
            try:
                while child.poll()is None:
                    sample=windows_memory(child.pid);samples.append(sample)
                    if sample['free_commit_kib']<3*1024*1024 or sample.get('process_private_bytes',0)>1024**3 or time.monotonic()-start>120:
                        child.kill();raise RuntimeError('Short analysis resource guard stopped owned child')
                    time.sleep(.1)
                if child.wait()!=0:raise RuntimeError('Analysis failed; inspect retained log')
            finally:
                if child.poll()is None:child.kill();child.wait()
        results.append(dict(mode=mode,wall_seconds=time.monotonic()-start,
            peak_private_bytes=max((s.get('process_peak_private_bytes',0)for s in samples),default=0),samples=samples))
    a=json.loads((directory/'tree.json').read_text(encoding='utf-8'));b=json.loads((directory/'stream.json').read_text(encoding='utf-8'))
    assert a==b,'Streaming analysis changed numerical results'
    write(directory/'comparison.json',dict(numerically_identical=True,full_horizon_run=False,results=results))
    print(json.dumps([{k:v for k,v in r.items()if k!='samples'}for r in results],indent=2))


if __name__=='__main__':main()
