import json,sys,tempfile,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from run_price_formation_linux import finish_result


class RunnerCompletionTests(unittest.TestCase):
    def test_resource_stop_with_clean_proxy_exit_is_not_a_success(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d)/'raw.json';p.write_bytes(b'')
            r=finish_result(dict(complete=False,exit_code=0,failure='resource guard'),p,dict(steps=4,seconds=1))
            self.assertFalse(r['complete']);self.assertEqual(r['failure'],'resource guard');self.assertEqual(r['partial_receipt_bytes'],0)

    def test_clean_proxy_exit_without_native_receipt_is_retained_as_failure(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d)/'raw.json';p.write_bytes(b'')
            r=finish_result(dict(complete=False,exit_code=0),p,dict(steps=4,seconds=1))
            self.assertFalse(r['complete']);self.assertIn('Invalid or incomplete',r['failure']);self.assertEqual(r['partial_receipt_bytes'],0)

    def test_truncated_horizon_fails_but_exact_clock_and_horizon_complete(self):
        with tempfile.TemporaryDirectory()as d:
            p=Path(d)/'raw.json'
            for steps,clock,expected in ((3,1,False),(4,2,False),(4,1,True)):
                p.write_text(json.dumps(dict(steps=steps,clock_step_seconds=clock,simulation_time_ms=steps*clock*1000,trades={'TEST':2})),encoding='utf-8')
                r=finish_result(dict(complete=False,exit_code=0),p,dict(steps=4,seconds=1));self.assertEqual(r['complete'],expected)


if __name__=='__main__':unittest.main()
