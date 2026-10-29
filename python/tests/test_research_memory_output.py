import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[2]/'scripts'))
from research_market_mechanisms import sha
from research_receipts import finish_result
from research_resources import stop_reason, windows_memory, reclaim_guest_file_cache


class ResearchMemoryTests(unittest.TestCase):
    def test_cache_reclaim_is_bounded_and_optional_failures_do_not_launch_unprotected(self):
        for count in [0,-1,2*1024**3]:
            with self.assertRaises(ValueError):reclaim_guest_file_cache(count)
        with patch('research_resources._reclaim_guest_file_cache',side_effect=OSError('unavailable')):
            result=reclaim_guest_file_cache()
        self.assertFalse(result['reclaimed']);self.assertIn('unavailable',result['error'])

    def test_hashes_stream_instead_of_reading_the_whole_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/'receipt.json';path.write_bytes(b'example')
            with patch.object(Path,'read_bytes',side_effect=AssertionError('unbounded read')):
                self.assertEqual(len(sha(path)),64)

    def test_native_summary_validates_large_receipt_without_reading_its_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt=Path(directory)/'receipt.json';summary=Path(directory)/'summary.json'
            receipt.write_bytes(b' '*5*1024*1024)
            summary.write_text(json.dumps(dict(schema=1,receipt_sha256=sha(receipt),steps=4,
                clock_step_seconds=1,simulation_time_ms=4000,trades={'TEST':2})))
            original=Path.read_text
            def bounded(path,*args,**kwargs):
                if path==receipt:raise AssertionError('large receipt parsed')
                return original(path,*args,**kwargs)
            with patch.object(Path,'read_text',bounded):
                result=finish_result(dict(complete=False,exit_code=0),receipt,dict(steps=4,seconds=1),summary)
            self.assertTrue(result['complete'])
            summary.write_text(summary.read_text().replace(sha(receipt),'0'*64))
            result=finish_result(dict(complete=False,exit_code=0),receipt,dict(steps=4,seconds=1),summary)
            self.assertFalse(result['complete']);self.assertIn('hash mismatch',result['failure'])

    def test_missing_summary_cannot_make_proxy_exit_zero_successful(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt=Path(directory)/'receipt.json';receipt.write_text('{}')
            result=finish_result(dict(complete=False,exit_code=0),receipt,dict(steps=4,seconds=1),Path(directory)/'missing')
            self.assertFalse(result['complete'])

    def test_private_limit_and_commit_limit_are_separate_from_physical_memory(self):
        sample=dict(free_commit_kib=2*1024*1024,host_available_physical_bytes=25*1024**3)
        self.assertEqual(stop_reason(sample,1),'host_commit_headroom_below_3GiB')
        sample.update(free_commit_kib=20*1024*1024,process_private_bytes=7*1024**3)
        self.assertEqual(stop_reason(sample,1),'owned_process_private_limit_exceeded')
        sample['process_private_bytes']=100
        self.assertIsNone(stop_reason(sample,1))
        self.assertEqual(stop_reason(sample,10801),'wall_timeout')

    @unittest.skipUnless(sys.platform=='win32','Windows counters')
    def test_direct_windows_sample_includes_own_private_commitment(self):
        sample=windows_memory(os.getpid())
        self.assertGreater(sample['host_commit_limit_bytes'],sample['host_commit_bytes'])
        self.assertGreater(sample['process_private_bytes'],0)
        self.assertGreater(sample['host_available_physical_bytes'],0)


if __name__=='__main__':unittest.main()
