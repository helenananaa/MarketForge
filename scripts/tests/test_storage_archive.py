import gzip
import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from storage_archive import FORMAT, archive_id, encoded, file_hash, recovery_json, verify_archive
from storage_clickhouse import ClickHouse


class ArchiveIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.directory = Path(self.temp.name)
        self.rows = {
            "marketforge_rooms": [{"room_id":"r","scenario_json":{},"status":"closed"}],
            "marketforge_executions": [self.execution(i) for i in range(3)],
            "marketforge_room_mutations": [],
        }
        self.write()

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def execution(seq):
        return {"room_id":"r","command_seq":seq,"participant_id":"p","account_id":"10",
                "command_json":{"NewOrder":{"order_id":seq+1}},
                "execution_json":{"accepted":False,"clearing_events":[{"amount":"170141183460469231731687303715884105727"}]}}

    def write(self):
        tables=[]
        for name,rows in self.rows.items():
            keys=["room_id","command_seq"] if name.endswith("executions") else ["room_id"]
            columns=list(rows[0]) if rows else ["room_id","mutation_seq"]
            data=b"".join(encoded(row) for row in rows)
            filename=name+".jsonl.gz"
            path=self.directory/filename
            path.write_bytes(gzip.compress(data,mtime=0))
            tables.append({"table":name,"columns":columns,"primary_key":keys,"file":filename,
                           "rows":len(rows),"raw_bytes":len(data),"sha256":hashlib.sha256(data).hexdigest(),
                           "compressed_bytes":path.stat().st_size,"compressed_sha256":file_hash(path)})
        self.manifest={"format":FORMAT,"room_id":"r","status":"closed","tables":tables}
        self.manifest["archive_id"]=archive_id(self.manifest)
        (self.directory/"manifest.json").write_bytes(encoded(self.manifest))

    def test_full_history_and_large_integer_strings_survive(self):
        self.assertEqual(verify_archive(self.directory)["last_command_seq"],2)
        recovery=recovery_json(self.directory)
        self.assertEqual(len(recovery["executions"]),3)
        self.assertFalse(recovery["executions"][0]["execution"]["accepted"])
        self.assertEqual(recovery["executions"][0]["execution"]["clearing_events"][0]["amount"],str(2**127-1))

    def test_compressed_corruption_rejected(self):
        path=self.directory/"marketforge_executions.jsonl.gz"
        data=bytearray(path.read_bytes());data[len(data)//2]^=1;path.write_bytes(data)
        with self.assertRaisesRegex(ValueError,"checksum"):
            verify_archive(self.directory)

    def test_rehashed_sequence_gap_rejected(self):
        self.rows["marketforge_executions"].pop(1);self.write()
        with self.assertRaisesRegex(ValueError,"sequence gap"):
            verify_archive(self.directory)

    def test_truncated_prefix_rejected(self):
        self.rows["marketforge_executions"].pop(0);self.write()
        with self.assertRaisesRegex(ValueError,"start at"):
            verify_archive(self.directory)

    def test_duplicate_primary_key_rejected(self):
        self.rows["marketforge_executions"][1]["command_seq"]=0;self.write()
        with self.assertRaisesRegex(ValueError,"primary key"):
            verify_archive(self.directory)

    def test_foreign_room_rejected_even_with_valid_checksums(self):
        self.rows["marketforge_executions"][0]["room_id"]="other";self.write()
        with self.assertRaisesRegex(ValueError,"scope"):
            verify_archive(self.directory)

    def test_manifest_tampering_rejected(self):
        self.manifest["room_id"]="other"
        (self.directory/"manifest.json").write_bytes(encoded(self.manifest))
        with self.assertRaisesRegex(ValueError,"digest"):
            verify_archive(self.directory)

    def test_archive_path_traversal_rejected(self):
        self.manifest["tables"][0]["file"]="../outside.gz"
        self.manifest["archive_id"]=archive_id(self.manifest)
        (self.directory/"manifest.json").write_bytes(encoded(self.manifest))
        with self.assertRaisesRegex(ValueError,"file path"):
            verify_archive(self.directory)

    def test_query_database_identifier_rejected(self):
        with self.assertRaisesRegex(ValueError,"database name"):
            ClickHouse("default; DROP TABLE x")


if __name__=="__main__":
    unittest.main()
