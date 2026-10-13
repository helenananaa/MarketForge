"""Completion acknowledgement cannot turn partial or legacy traces into proof."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from benchmark_market_load import finish_server
from bot_runtime_receipts import audit_receipts


class FakeProcess:
    pid = 12345

    def __init__(self, complete=None, timeout=False):
        self.returncode = None
        self.complete = complete
        self.timeout = timeout
        self.terminated = self.killed = False

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminated = True
        self.returncode = -1

    def kill(self):
        self.killed = True
        self.returncode = -9

    def wait(self, timeout=None):
        if self.returncode is None:
            if self.timeout:
                raise subprocess.TimeoutExpired('fake-server', timeout)
            if self.complete:
                self.complete()
            self.returncode = 0
        return self.returncode


class TraceShutdownTests(unittest.TestCase):
    def test_legacy_binary_needs_no_ack_and_keeps_legacy_cleanup(self):
        process = FakeProcess()
        receipt = finish_server(process)
        self.assertTrue(process.terminated)
        self.assertFalse(receipt['requested'])
        self.assertIsNone(receipt['qualified'])

    def test_success_requires_exact_file_identity_size_and_eof(self):
        for fault in (None, 'pid', 'path', 'bytes', 'errors', 'partial', 'extra', 'missing'):
            with self.subTest(fault=fault), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                trace = root / 'trace.jsonl'
                request = root / 'shutdown.request'
                def complete():
                    self.assertTrue(request.exists())
                    marker = {'schema_version':1, 'stage':'trace_complete', 'wall_us':10,
                              'process_id':12345, 'decision_sequence':1, 'trace_errors':0}
                    trace.write_text(json.dumps(marker) + '\n', encoding='utf-8')
                    ack = {'version':1, 'trace_path':str(trace), 'trace_bytes':trace.stat().st_size,
                           'marker':dict(marker)}
                    if fault == 'pid': ack['marker']['process_id'] += 1
                    if fault == 'path': ack['trace_path'] = str(root / 'other.jsonl')
                    if fault == 'bytes': ack['trace_bytes'] -= 1
                    if fault == 'errors': ack['marker']['trace_errors'] = 1
                    if fault in ('partial', 'extra'):
                        with trace.open('a', encoding='utf-8') as stream:
                            stream.write('{"' if fault == 'partial' else '{}\n')
                        ack['trace_bytes'] = trace.stat().st_size
                    if fault != 'missing':
                        Path(str(trace) + '.complete.json').write_text(json.dumps(ack), encoding='utf-8')
                process = FakeProcess(complete)
                receipt = finish_server(process, request, trace)
                self.assertEqual(receipt['qualified'], fault is None)
                self.assertFalse(process.terminated or process.killed)

    def test_timeout_kills_and_keeps_failure_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            process = FakeProcess(timeout=True)
            root = Path(directory)
            receipt = finish_server(process, root / 'shutdown.request', root / 'trace.jsonl', .01)
            self.assertFalse(receipt['qualified'])
            self.assertTrue(receipt['forced_kill'] and process.killed)
            self.assertIn('TimeoutExpired', receipt['error'])

    def test_matching_ack_never_excuses_a_malformed_interior_trace_row(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            trace = root / 'trace.jsonl'
            request = root / 'shutdown.request'
            def complete():
                marker = {'schema_version':1, 'stage':'trace_complete', 'wall_us':10,
                          'process_id':12345, 'decision_sequence':0, 'trace_errors':0}
                trace.write_text('{"\n' + json.dumps(marker) + '\n', encoding='utf-8')
                ack = {'version':1, 'trace_path':str(trace), 'trace_bytes':trace.stat().st_size,
                       'marker':marker}
                Path(str(trace) + '.complete.json').write_text(json.dumps(ack), encoding='utf-8')
            self.assertTrue(finish_server(FakeProcess(complete), request, trace)['qualified'])
            audit = audit_receipts(trace)
            self.assertTrue(audit['errors'])
            self.assertFalse(audit['qualified'])


if __name__ == '__main__':
    unittest.main()
