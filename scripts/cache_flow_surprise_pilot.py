"""Copy verified seed41 evidence without modifying frozen main recipes or plan."""
import json
import shutil
from research_market_mechanisms import sha, write
from research_flow_surprise import BASE


def cache():
    source, target = BASE/'pilot', BASE/'main'
    old, new = [json.loads((p/'plan.json').read_text(encoding='utf-8')) for p in (source, target)]
    assert old['binary_sha256'] == new['binary_sha256']
    result = {}
    for row in old['runs']:
        match = next(r for r in new['runs'] if r['name'] == row['name'])
        assert row['recipe_sha256'] == match['recipe_sha256']
        name = row['name']
        done = json.loads((source/(name+'.run.json')).read_text(encoding='utf-8'))
        assert done['complete']
        assert sha(source/(name+'.receipt.json')) == done['receipt_sha256']
        for suffix in ('.run.json', '.receipt.json', '.summary.json', '.log'):
            assert not (target/(name+suffix)).exists()
            shutil.copyfile(source/(name+suffix), target/(name+suffix))
        result[name] = dict(directory=str(source), run_sha256=sha(source/(name+'.run.json')),
            receipt_sha256=done['receipt_sha256'], note='Exact seed41 pilot artifact reuse, not another native run. Execution archive paths remain in the original pilot spool directory.')
    assert not (target/'cache-provenance.json').exists()
    write(target/'cache-provenance.json', result)


if __name__ == '__main__':
    cache()
