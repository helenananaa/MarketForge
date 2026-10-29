import json
from pathlib import Path
import sys
import os
import subprocess
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from research_json import Receipt, compact_quote
from analyze_market_mechanisms import analyze_run, sha


class ReceiptStreamingTests(unittest.TestCase):
    def test_numeric_defaults_are_process_local_and_keep_explicit_overrides(self):
        script=Path(__file__).resolve().parents[2]/'scripts'
        code=f"import sys,os,json;sys.path.insert(0,{str(script)!r});import research_numeric;print(os.environ['OPENBLAS_NUM_THREADS'])"
        env=dict(os.environ)
        for key in ['OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','MKL_NUM_THREADS']:env.pop(key,None)
        self.assertEqual(subprocess.check_output([sys.executable,'-c',code],env=env,text=True).strip(),'1')
        env['OPENBLAS_NUM_THREADS']='7'
        self.assertEqual(subprocess.check_output([sys.executable,'-c',code],env=env,text=True).strip(),'7')

    def test_top_level_fields_ignore_nested_names_and_escaped_delimiters(self):
        value=dict(nested={'states':[9],'text':'中文 \\" } ['},states=[{'v':[1,2]},None,True,1.25e30],steps=2)
        with tempfile.TemporaryDirectory()as directory:
            path=Path(directory)/'receipt';path.write_text(json.dumps(value,ensure_ascii=False),encoding='utf-8')
            receipt=Receipt(path)
            self.assertEqual(receipt['nested'],value['nested'])
            self.assertEqual(list(receipt['states']),value['states'])
            self.assertEqual(len(receipt['states']),4)
            self.assertEqual(receipt['steps'],2)

    def test_rejects_unclosed_collections_duplicate_roots_and_trailing_data(self):
        with tempfile.TemporaryDirectory()as directory:
            path=Path(directory)/'receipt'
            for raw in ['{"v":[{}','{"v":1,"v":2}','{"v":1}garbage','{"v":1,}']:
                path.write_text(raw)
                with self.assertRaises(ValueError):Receipt(path)

    def test_compact_quotes_keep_exact_depth_and_midpoint_inputs(self):
        quote=dict(time_ms=0,book=dict(bids=[dict(price_tick=10000,qty=3),dict(price_tick=9990,qty=7),dict(price_tick=9900,qty=20)],
                                      asks=[dict(price_tick=10002,qty=5),dict(price_tick=10010,qty=11),dict(price_tick=10100,qty=20)]))
        result=compact_quote(quote)
        self.assertEqual(result['book']['bids'],[dict(price_tick=10000,qty=10)])
        self.assertEqual(result['book']['asks'],[dict(price_tick=10002,qty=16)])

    def test_analysis_matches_full_tree_without_loading_full_receipt(self):
        with tempfile.TemporaryDirectory()as directory:
            out=Path(directory);recipe=out/'recipe.json';recipe.write_text('{"agents":[]}')
            data=dict(steps=2,clock_step_seconds=1,simulation_time_ms=2000,bots=0,states=[],
                      assets_before={'accounts':[]},assets_after={'accounts':[]},rejections={},trades={'M':2},
                      trade_receipts=[dict(instrument_id='M',trade_id=n+1,time_ms=500+n*1000,qty=1,price_tick=100+n,
                        maker_account_id=10,taker_account_id=20,taker_order_id=n+1,taker_side='Buy')for n in range(2)],
                      book_snapshots=[dict(instrument_id='M',time_ms=n*1000,book=dict(
                        bids=[dict(price_tick=100,qty=2)],asks=[dict(price_tick=101,qty=3)]))for n in range(2)])
            receipt=out/'tiny.receipt.json';receipt.write_text(json.dumps(data))
            (out/'tiny.run.json').write_text(json.dumps(dict(complete=True,receipt_sha256=sha(receipt),inputs={'binary_sha256':'frozen'},wall_seconds=1)))
            row=dict(name='tiny',recipe=str(recipe),recipe_sha256=sha(recipe),steps=2,seconds=1,bots=0,warmup_ms=0)
            with patch('analyze_market_mechanisms.Receipt',return_value=data):expected=analyze_run(out,row)
            original=Path.read_text
            def bounded(path,*args,**kwargs):
                if path==receipt:raise AssertionError('Whole receipt loaded')
                return original(path,*args,**kwargs)
            with patch.object(Path,'read_text',bounded):actual=analyze_run(out,row)
            self.assertEqual(actual,expected)


if __name__=='__main__':unittest.main()
