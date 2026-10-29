"""Compare full observed market paths separately from floating-point state bytes."""
import json,gc
from pathlib import Path
from research_market_mechanisms import sha,write
from research_price_formation import BASE


def compare():
    out=BASE/'linux-compatibility';results=[]
    for profile in ('control','anchor-slope'):
        name=f'native-s41-{profile}';old=BASE/'pilot'/(name+'.receipt.json');new=out/(name+'.receipt.json')
        a=json.loads(old.read_text(encoding='utf-8'));b=json.loads(new.read_text(encoding='utf-8'));checks={k:a[k]==b[k]for k in a if k!='states'}
        diffs=[]
        def walk(x,y,path):
            if x==y:return
            if isinstance(x,dict)and isinstance(y,dict):
                assert x.keys()==y.keys(),path
                for k in x:walk(x[k],y[k],path+'.'+k)
            elif isinstance(x,list)and isinstance(y,list):
                assert len(x)==len(y),path
                for i,(v,w)in enumerate(zip(x,y)):walk(v,w,path+f'[{i}]')
            elif isinstance(x,float)and isinstance(y,float):diffs.append(dict(path=path,windows=x,linux=y,absolute_difference=abs(x-y)))
            else:raise AssertionError((path,x,y))
        walk(a['states'],b['states'],'states')
        results.append(dict(profile=profile,windows_sha256=sha(old),linux_sha256=sha(new),byte_identical=sha(old)==sha(new),non_state_checks=checks,
            float_state_differences=len(diffs),max_abs_float_difference=max((r['absolute_difference']for r in diffs),default=0),largest_examples=sorted(diffs,key=lambda r:r['absolute_difference'],reverse=True)[:10]))
        assert all(checks.values()),checks
        del a,b;gc.collect()
    write(BASE/'platform-compatibility.json',dict(records=results,scope='Same compiler version/source/recipe. Every4h trade, book sample, assets, action/fill/rejection count and non-state field compared. Raw receipt bytes differ in final float state; listed differences are not normalized away. Fresh full Linux controls are required; do not claim universal cross-platform trajectory equality.'))
    print(json.dumps(results,indent=2))


if __name__=='__main__':compare()
