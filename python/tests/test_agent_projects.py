"""Project, dependency recovery, research boundary and real-library acceptance."""
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from marketforge.agents.projects import normalize_project, project_version
from marketforge.agents.research import Research, public_addresses
from marketforge.agents.runtime import Runtime
from marketforge.agents.sandbox import DockerSandbox
from python.tests import test_agent_runtime as fixtures
from python.tests.test_agent_runtime import PLUGIN


class ProjectTests(unittest.TestCase):
    setUp = fixtures.RuntimeTests.setUp
    call = fixtures.RuntimeTests.call
    buy = fixtures.RuntimeTests.buy

    def tearDown(self):
        self.runtime.stop("alice")
        for _, thread in self.runtime.installers.values():
            thread.join(3)
        self.runtime.store.db.close()
        self.directory.cleanup()

    def save_project(self, requirements=None):
        return self.call("project", "strategy_save", {"name": "research", "files": {
            "strategy.py": "from signals import value\ndef decide(observations,state): return {'actions': [], 'state': {'value': value()}}",
            "signals.py": "def value(): return 2"}, "requirements": requirements or [], "interval_seconds": 2})

    def test_multifile_patch_preserves_old_version_and_dependencies(self):
        first = self.save_project(["numpy==2.1.3"])
        second = self.call("patch", "strategy_patch", {"name": "research", "files": {"signals.py": "def value(): return 3", "data/prices.json": "[1,2,3]"}})
        self.assertNotEqual(first["version"], second["version"])
        old = self.call("old", "strategy_read", {"name": "research", "version": first["version"]})
        current = self.call("read", "strategy_read", {"name": "research"})
        self.assertIn("return 2", old["files"]["signals.py"])
        self.assertIn("return 3", current["files"]["signals.py"])
        self.assertEqual(current["requirements"], ["numpy==2.1.3"])

    def test_paths_and_pip_options_never_reach_host(self):
        for path in ("../escape.py", "/etc/passwd", "C:\\secret", "a/../b.py", "a//b.py"):
            with self.assertRaises(ValueError):
                normalize_project({"files": {path: "x"}, "entrypoint": path})
        for req in ("--index-url https://x", "-r /etc/passwd", "../local", "numpy\n--target=/host"):
            with self.assertRaises(ValueError):
                normalize_project({"code": "", "requirements": [req]})
        for req in ("pandas", "numpy>=1.26", "requests[socks]>=2", "thing @ https://example.com/thing.whl"):
            self.assertEqual(normalize_project({"code": "", "requirements": [req]})["requirements"], [req])

    def test_install_is_async_trade_continues_and_result_is_bound_to_version(self):
        entered, release = threading.Event(), threading.Event()
        def prepare(project, cancel=None, progress=None):
            entered.set(); progress("resolving numpy")
            release.wait(2)
            return {"image": "sha256:fixture", "packages": ["numpy==2.1.3"], "log": "done"}
        self.sandbox.prepare = prepare
        first = self.save_project(["numpy==2.1.3"])
        job = self.call("install", "strategy_install", {"name": "research"})
        self.assertTrue(entered.wait(1))
        self.assertTrue(self.call("trade", "trade", self.buy())["accepted"])
        self.assertIn("error", self.call("edit", "strategy_patch", {"name": "research", "requirements": ["pandas"]}))
        release.set()
        self.runtime.installers[job["id"]][1].join(3)
        current = self.call("status", "strategy_status", {"name": "research"})
        self.assertEqual(current["installation"]["status"], "ready")
        self.assertEqual(current["version"], first["version"])
        self.assertTrue(self.runtime.wake_events["alice"].is_set())

    def test_pause_cancels_install_and_prevents_activation(self):
        entered = threading.Event()
        def prepare(project, cancel=None, progress=None):
            entered.set(); cancel.wait(2)
            return {"image": "sha256:late", "packages": [], "log": "late"}
        self.sandbox.prepare = prepare
        self.save_project(["pandas"])
        job = self.call("install", "strategy_install", {"name": "research"})
        self.assertTrue(entered.wait(1))
        self.runtime.stop("alice")
        self.runtime.installers[job["id"]][1].join(3)
        self.assertEqual(self.runtime.install_status(self.runtime.strategies("alice")[0])["status"], "cancelled")
        self.assertIsNone(self.runtime.strategies("alice")[0]["environment"])

    def test_failed_install_is_visible_and_retryable(self):
        def prepare(*args, **kwargs):
            raise ValueError("No matching distribution found for typo")
        self.sandbox.prepare = prepare
        self.save_project(["typo"])
        job = self.call("install", "strategy_install", {"name": "research"})
        self.runtime.installers[job["id"]][1].join(3)
        self.assertIn("No matching", self.runtime.install_status(self.runtime.strategies("alice")[0])["error"])
        result = self.call("fix", "strategy_patch", {"name": "research", "requirements": ["numpy"]})
        self.assertNotIn("error", {k: v for k, v in result.items() if v is not None})

    def test_editing_only_code_reuses_the_installed_environment(self):
        self.sandbox.prepare = lambda *a, **kw: {"image": "sha256:cached", "packages": ["numpy==2.1.3"], "log": "done"}
        self.save_project(["numpy==2.1.3"])
        job = self.call("install", "strategy_install", {"name": "research"})
        self.runtime.installers[job["id"]][1].join(3)
        changed = self.call("edit", "strategy_patch", {"name": "research", "files": {"signals.py": "def value(): return 9"}})
        self.assertEqual(changed["environment"], "sha256:cached")
        self.assertEqual(self.call("install-again", "strategy_install", {"name": "research"})["id"], job["id"])

    def test_research_does_not_hold_trading_lock(self):
        entered, release = threading.Event(), threading.Event()
        def read(url):
            entered.set(); release.wait(2); return {"text": "external data"}
        self.runtime.research.read = read
        reader = threading.Thread(target=self.call, args=("read", "web_read", {"url": "https://example.com/"}))
        reader.start(); self.assertTrue(entered.wait(1))
        try:
            self.assertTrue(self.runtime.lock("alice").acquire(timeout=0.2))
            self.runtime.lock("alice").release()
            self.assertTrue(self.call("trade", "trade", self.buy())["accepted"])
        finally:
            release.set(); reader.join(3)

    def test_restart_marks_build_interrupted_and_does_not_reactivate(self):
        self.runtime.store.put("install_job", "job", {"id": "job", "trader": "alice", "status": "running"})
        self.runtime.store.db.close()
        self.runtime = Runtime(self.directory.name, PLUGIN, "http://unused", self.sandbox, lambda _: self.exchange)
        self.assertEqual(self.runtime.store.get("install_job", "job")["status"], "interrupted")


class ResearchTests(unittest.TestCase):
    def test_private_and_mixed_dns_are_rejected(self):
        for addresses in (["127.0.0.1"], ["10.0.0.1"], ["169.254.169.254"], ["::1"], ["93.184.216.34", "192.168.1.1"]):
            with patch("socket.getaddrinfo", return_value=[(2, 1, 6, "", (a, 443)) for a in addresses]):
                with self.assertRaises(ValueError):
                    public_addresses("source.example", 443)

    def test_source_receipt_html_and_search_results(self):
        research = Research()
        research.fetch = lambda _: {"raw": "<html><script>hidden()</script><h1>Market update</h1><a href='/news'>Next</a></html>", "url": "https://example.com", "content_type": "text/html", "retrieved_at": "now", "sha256": "hash"}
        result = research.read("https://example.com")
        self.assertNotIn("hidden()", result["text"])
        self.assertEqual(result["links"], ["https://example.com/news"])
        research.fetch = lambda _: {"raw": '<rss><channel><item><title>News</title><link>https://example.com</link><description>Summary</description></item></channel></rss>', "content_type": "application/rss+xml"}
        self.assertEqual(research.search("markets")["results"][0]["title"], "News")


class ProjectLiveTests(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("MARKETFORGE_AGENT_PROJECT_TEST") == "1", "set MARKETFORGE_AGENT_PROJECT_TEST=1 to install real packages")
    def test_install_real_numpy_pandas_system_library_and_multifile_analysis(self):
        project = normalize_project({"files": {
            "strategy.py": "from signals import mean\ndef decide(observations,state): return {'actions': [], 'state': {'mean': mean()}}",
            "signals.py": "import numpy as np\nimport pandas as pd\ndef mean(): return float(pd.Series(np.array([1,2,3])).mean())",
            "data/config.json": '{"window":3}'}, "requirements": ["numpy==2.1.3", "pandas==2.2.3"], "system_packages": ["libgomp1"]})
        sandbox = DockerSandbox()
        receipt = sandbox.prepare(project)
        self.assertIn("numpy==2.1.3", receipt["packages"])
        self.assertTrue(any(p.startswith("libgomp1=") for p in receipt["system_packages"]))
        self.assertEqual(sandbox.run_project(project, receipt, {}, {})["state"]["mean"], 2.0)
        analysis = sandbox.run_project(project, receipt, {}, {}, analysis="from signals import mean\nprint('analysis complete')\nresult={'mean':mean()}")
        self.assertEqual(analysis["result"], {"mean": 2.0})
        self.assertIn("analysis complete", analysis["output"])
        self.assertFalse(analysis["orders_submitted"])
        # Persist a reusable receipt rather than claiming the original requirement string is a lock.
        output = Path(__file__).resolve().parents[2] / "target/agent-project-acceptance.json"
        output.write_text(json.dumps({"environment": receipt, "project_version": project_version(project), "analysis": analysis}, indent=2))


if __name__ == "__main__":
    unittest.main()
