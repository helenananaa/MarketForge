import json,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
import analyze_parent_liquidity_long as analysis


class LongPrefixTests(unittest.TestCase):
    def check(self,old,new):
        with tempfile.TemporaryDirectory()as directory:
            root=Path(directory);control=root/'controls';control.mkdir();long=root/'long';long.mkdir()
            row=dict(seed=7,profile='legacy',name='trial')
            (control/'plan.json').write_text(json.dumps(dict(runs=[row])),encoding='utf-8')
            for folder,trades in ((control,old),(long,new)):
                (folder/'trial.receipt.json').write_text(json.dumps(dict(trade_receipts=trades),separators=(',',':')),encoding='utf-8')
            with patch.object(analysis,'BASE',root):return analysis.prefix(long,row)

    def test_exact_ordered_prefix_accepts_continuation_after_boundary(self):
        old=[dict(time_ms=1,qty=2),dict(time_ms=14400000,qty=1)]
        result=self.check(old,old+[dict(time_ms=14401000,qty=3)])
        self.assertEqual(result['trade_prefix_records'],2)
        self.assertTrue(result['byte_value_identical'])

    def test_changed_fill_in_prior_horizon_is_rejected(self):
        with self.assertRaises(AssertionError):
            self.check([dict(time_ms=1,qty=2)],[dict(time_ms=1,qty=3)])

    def test_extra_fill_before_old_horizon_is_rejected(self):
        old=[dict(time_ms=1,qty=2)]
        with self.assertRaises(AssertionError):self.check(old,old+[dict(time_ms=14399000,qty=1)])


if __name__=='__main__':unittest.main()
