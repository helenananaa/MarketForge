"""Analyze complete pairs only after immutable native completion metadata exists."""
import json
import subprocess
import sys
import time
from research_flow_surprise import BASE, ROOT


def run():
    seen = {name: 0 for name in ('main', 'holdout')}
    finished = set()
    while len(finished)<2:
        for name in ('main', 'holdout'):
            if name in finished:
                continue
            folder = BASE/name
            plan = json.loads((folder/'plan.json').read_text(encoding='utf-8'))
            complete = []
            for row in plan['runs']:
                done = folder/(row['name']+'.run.json')
                if done.exists():
                    assert json.loads(done.read_text(encoding='utf-8'))['complete']
                    complete.append(row)
            pairs = sum(sum(r['seed']==s for r in complete)==2 for s in {r['seed'] for r in plan['runs']})
            if pairs>seen[name]:
                with (BASE/(name+f'-analysis-{pairs}-pairs.log')).open('wb') as log:
                    for script, arguments in [('analyze_flow_surprise.py', []), ('verify_flow_surprise.py', ['diagnostics']), ('diagnose_flow_surprise_horizons.py', [])]:
                        subprocess.run([sys.executable, str(ROOT/'scripts'/script), *arguments, str(folder)], stdout=log, stderr=subprocess.STDOUT, check=True, cwd=ROOT)
                print(f'{name}: {pairs} complete pairs analyzed', flush=True)
                seen[name] = pairs
            if len(complete)==len(plan['runs']):
                finished.add(name)
        if len(finished)<2:
            time.sleep(15)


if __name__ == '__main__':
    run()
