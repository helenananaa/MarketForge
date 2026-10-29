import json,sys,tempfile,unittest
from collections import Counter
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from analyze_wick_basis_capacity import records,one
from analyze_market_mechanisms import sha


class BasisCapacityAnalysisTests(unittest.TestCase):
    def test_truncated_array_headers_fail_without_waiting_forever(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d)/'broken.json'
            for text in ('{"items":','{"items":[{"q":'):
                p.write_text(text,encoding='utf-8')
                with self.assertRaises((ValueError,json.JSONDecodeError)):list(records(p,'items'))

    def test_streaming_handles_large_unicode_records_and_nested_delimiters(self):
        values=[{'text':'中'*400000+'"[,]\\','nested':[{'x':[1,2]}]},{'ok':True}]
        with tempfile.TemporaryDirectory()as d:
            p=Path(d)/'sample.json';p.write_text(json.dumps({'other':'unrelated','items':values,'tail':[]},ensure_ascii=False),encoding='utf-8')
            self.assertEqual(list(records(p,'items')),values)

    def test_real_fill_ledger_includes_exposure_carried_into_primary_and_boundary(self):
        with tempfile.TemporaryDirectory()as d:
            out=Path(d);recipe={'scenario':{'accounts':[{'Spot':{'account_id':42,'position_qty':10}}]},'agents':[]}
            for leg,market,peer in [('Perp','P','S'),('Spot','S','P')]:
                recipe['agents'].append({'Plugin':{'participant':{'participant_id':leg,'account_id':42,'instrument_id':market},'plugin_id':'BasisLinkTrader','config':{'leg':leg,'hedge_instrument_id':peer,'neutral_spot':10,'max_hedge':10,'entry_ppm':2000}}})
            spec=out/'toy.recipe.json';spec.write_text(json.dumps(recipe),encoding='utf-8')
            trades=[{'time_ms':t,'instrument_id':m,'taker_side':s,'qty':q,'taker_account_id':42,'maker_account_id':7}for t,m,s,q in [(9000,'P','Buy',10),(11000,'S','Sell',10),(15000,'P','Sell',4),(17000,'S','Buy',4),(20000,'S','Buy',1)]]
            raw=out/'toy.receipt.json';raw.write_text(json.dumps({'trade_receipts':trades,'states':[{'id':leg,'state':{'Plugin':{'data':{}}}}for leg in ('Perp','Spot')]}),encoding='utf-8')
            (out/'toy.run.json').write_text(json.dumps({'complete':True,'receipt_sha256':sha(raw),'trades':dict(Counter(t['instrument_id']for t in trades))}),encoding='utf-8')
            row=dict(name='toy',recipe=str(spec),recipe_sha256=sha(spec),seed=1,profile='test',warmup_ms=10000,steps=20)
            result=one(out,row);self.assertEqual(len(result),1);r=result[0]
            self.assertEqual(r['at_cap_ms'],5000);self.assertEqual(r['unhedged_ms'],3000)
            self.assertEqual(r['max_abs_residual'],10,'risk held before warmup must count')
            self.assertEqual(r['max_abs_perp'],10);self.assertEqual(r['end_residual'],1,'final boundary has no primary duration but changes final position')
            self.assertEqual(r['spot'],5);self.assertEqual(r['perp'],6)


if __name__=='__main__':unittest.main()
