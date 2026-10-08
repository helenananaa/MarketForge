"""Real local HTTP acceptance without a model or database dependency."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from background_market import recipe
from marketforge import Client


class BackgroundRecipeTests(unittest.TestCase):
    def test_room_remapping_seeds_and_finite_exclusive_accounts(self):
        first = recipe("one", 7, False)
        second = recipe("two", 19)
        self.assertEqual(len(first["agents"]), 20)
        self.assertEqual(len({a["Plugin"]["participant"]["account_id"] for a in first["agents"]}), 20)
        for a, b in zip(first["agents"], second["agents"]):
            self.assertEqual(a["Plugin"]["participant"]["room_id"], "one")
            self.assertEqual(b["Plugin"]["participant"]["room_id"], "two")
            self.assertNotEqual(a["Plugin"]["seed"], b["Plugin"]["seed"])
            self.assertLessEqual(a["Plugin"]["seed"], 2**53 - 1)
        self.assertFalse(first["autostart_agents"])


class BackgroundHttpTests(unittest.TestCase):
    def test_catalog_room_autostart_fills_and_saved_bot_state(self):
        server = Path(os.environ.get("MARKETFORGE_TEST_SERVER", str(
            ROOT / "target/debug" / ("exchange-server.exe" if os.name == "nt" else "exchange-server"))))
        if not server.exists():
            self.skipTest("build exchange-server before HTTP acceptance")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("MARKETFORGE_")}
        env["MARKETFORGE_BIND_ADDR"] = f"127.0.0.1:{port}"
        env["MARKETFORGE_AUTH_TOKENS_JSON"] = '{"background-test":"background-owner"}'
        env["MARKETFORGE_BOT_PLUGIN_DIR"] = str(ROOT / "bot-plugins")
        receipts = ROOT / ".local"
        receipts.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="marketforge-background-") as directory:
            with (receipts / "background-market-http.log").open("w", encoding="utf-8") as log:
                process = subprocess.Popen([str(server)], cwd=directory, env=env, stdout=log, stderr=subprocess.STDOUT)
                try:
                    client = Client(f"http://127.0.0.1:{port}", bearer="background-test", timeout=10)
                    deadline = time.monotonic() + 20
                    while True:
                        try:
                            client.health_ready()
                            break
                        except (RuntimeError, OSError):
                            if process.poll() is not None or time.monotonic() >= deadline:
                                self.fail("isolated server failed readiness; see .local/background-market-http.log")
                            time.sleep(0.05)
                    catalog = {b["id"]: b for b in client.list_bots()}
                    for bot in recipe()["agents"]:
                        self.assertIn(bot["Plugin"]["plugin_id"], catalog)
                    spec = recipe("http-background-test")
                    spec["agent_interval_ms"] = 25
                    client._request("POST", "/rooms", spec)
                    deadline = time.monotonic() + 15
                    while True:
                        saved = client.room_bots("http-background-test")
                        status = client._request("GET", "/rooms/http-background-test/agents")
                        response = client.observe("http-background-test", 20)
                        observation = response.get("observation", response)
                        if observation["step"] >= 80 and observation["public_trades"]:
                            break
                        if time.monotonic() >= deadline:
                            self.fail("background market failed to produce public trades")
                        time.sleep(0.05)
                    self.assertTrue(status["running"])
                    self.assertFalse(status["bot_errors"])
                    self.assertEqual(len(saved["agents"]), 20)
                    self.assertTrue(all(a["kind_state"]["Plugin"]["data"] is not None for a in saved["agents"]))
                    self.assertTrue(all(t["maker_account_id"] != t["taker_account_id"] for t in observation["public_trades"]))
                    (receipts / "background-market-http.json").write_text(json.dumps({
                        "status": status, "clock_step": observation["step"], "public_trades": observation["public_trades"],
                        "bot_count": len(saved["agents"]), "catalog": sorted(catalog)}, indent=2), encoding="utf-8")
                    client.pause_room("http-background-test")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
