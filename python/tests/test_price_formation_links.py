import json,sys,tempfile,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from analyze_price_formation_links import one
from research_market_mechanisms import sha


class SameWaveLinkTests(unittest.TestCase):
    def fixture(self,out,samples):
        recipe=out/'recipe.json';recipe.write_text(json.dumps(dict(agents=[dict(Plugin=dict(plugin_id='BasisLinkTrader',config=dict(entry_ppm=1000)))])),encoding='utf-8')
        receipt=out/'case.receipt.json';receipt.write_text(json.dumps(dict(book_snapshots=samples)),encoding='utf-8')
        (out/'case.run.json').write_text(json.dumps(dict(complete=True,receipt_sha256=sha(receipt))),encoding='utf-8')
        return dict(name='case',seed=1,profile='fixture',steps=3,warmup_ms=0,recipe=str(recipe))

    def sample(self,t,market,bid,ask):
        return dict(time_ms=t,instrument_id=market,phase='after_all_actions',book=dict(bids=[dict(price_tick=bid,qty=10)]if bid else[],asks=[dict(price_tick=ask,qty=10)]if ask else[]))

    def test_missing_side_is_counted_and_breaks_persistence_without_forward_fill(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d);samples=[]
            for t in (0,1000,2000):
                samples += [self.sample(t,'V-BTC-SPOT',10000,10002),self.sample(t,'V-BTC-PERP',10020 if t!=1000 else None,10022)]
            r=one(p,self.fixture(p,samples))
            self.assertEqual(r['available_waves'],2);self.assertEqual(r['missing_two_sided_waves'],1)
            self.assertAlmostEqual(r['gross_pair_edge_bp']['median'],(10020/10002-1)*10000)
            self.assertEqual(r['thresholds'][0]['fraction_above'],1.)
            self.assertEqual(r['thresholds'][0]['longest_run_seconds'],1)
            self.assertEqual(r['absolute_minute_end_mid_basis_bp']['samples'],0)

    def test_unmatched_market_timestamps_fail_instead_of_using_a_stale_peer(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d);samples=[self.sample(0,'V-BTC-SPOT',10000,10002),self.sample(1000,'V-BTC-PERP',10020,10022)]
            with self.assertRaises(AssertionError):one(p,self.fixture(p,samples))


if __name__=='__main__':unittest.main()
