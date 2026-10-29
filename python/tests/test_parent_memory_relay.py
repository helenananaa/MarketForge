import json,sys,tempfile,unittest
from pathlib import Path
from unittest.mock import patch
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from parent_memory_relay import tick
from parent_resource_guard import relay_sample


class PublishTests(unittest.TestCase):
    def test_sharing_collision_retains_old_pulse_then_recovers_with_fresh_sample(self):
        with tempfile.TemporaryDirectory()as directory:
            base=Path(directory);target=base/'host-memory.json';old=dict(unix_seconds=10,free_commit_kib=9)
            target.write_text(json.dumps(old))
            sample=lambda:dict(free_commit_kib=7*1024**2,host_available_physical_bytes=8*1024**3)
            with patch.object(Path,'replace',side_effect=PermissionError('sharing collision')):
                self.assertFalse(tick(base,sample,lambda:20)['published'])
            self.assertEqual(json.loads(target.read_text()),old)
            self.assertTrue(tick(base,sample,lambda:21)['published'])
            self.assertEqual(json.loads(target.read_text())['unix_seconds'],21)

    def test_persistent_publication_failure_does_not_refresh_stale_target(self):
        with tempfile.TemporaryDirectory()as directory:
            base=Path(directory);target=base/'host-memory.json'
            target.write_text(json.dumps(dict(unix_seconds=10,free_commit_kib=7*1024**2)))
            sample=lambda:dict(free_commit_kib=7*1024**2,host_available_physical_bytes=8*1024**3)
            with patch.object(Path,'replace',side_effect=PermissionError('persistent')):
                for now in (11,20,41):self.assertFalse(tick(base,sample,lambda:now)['published'])
            self.assertFalse(relay_sample(json.loads(target.read_text()),41)['host_heartbeat_valid'])


if __name__=='__main__':unittest.main()
