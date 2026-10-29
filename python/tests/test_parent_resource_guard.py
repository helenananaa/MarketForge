import sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from parent_resource_guard import relay_sample


class RelayClockTests(unittest.TestCase):
    def test_small_future_skew_is_fresh_but_raw_evidence_is_retained(self):
        x=relay_sample(dict(unix_seconds=100.16,free_commit_kib=7*1024**2),100)
        self.assertTrue(x['host_heartbeat_valid']);self.assertTrue(x['host_clock_skew_clamped'])
        self.assertAlmostEqual(x['host_heartbeat_raw_age_seconds'],-.16)
        self.assertEqual(x['host_heartbeat_age_seconds'],0)
        self.assertEqual(x['host_free_commit_kib'],7*1024**2)

    def test_stale_or_large_future_timestamp_still_fails_closed(self):
        for stamp in (70,69,102.01):
            x=relay_sample(dict(unix_seconds=stamp,free_commit_kib=100*1024**2),100)
            self.assertFalse(x['host_heartbeat_valid']);self.assertEqual(x['host_free_commit_kib'],0)
        self.assertTrue(relay_sample(dict(unix_seconds=102,free_commit_kib=1),100)['host_heartbeat_valid'])

    def test_future_skew_never_masks_real_commitment_pressure(self):
        x=relay_sample(dict(unix_seconds=100.16,free_commit_kib=2*1024**2),100)
        self.assertTrue(x['host_heartbeat_valid'])
        self.assertLess(x['host_free_commit_kib'],3*1024**2)

    def test_missing_corrupt_and_nonfinite_values_are_invalid(self):
        for pulse in ({},dict(unix_seconds=float('nan'),free_commit_kib=3),dict(unix_seconds=100,free_commit_kib=-1)):
            x=relay_sample(pulse,100);self.assertFalse(x['host_heartbeat_valid']);self.assertEqual(x['host_free_commit_kib'],0)


if __name__=='__main__':unittest.main()
