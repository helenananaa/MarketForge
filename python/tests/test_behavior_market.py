"""Recipe consistency and isolated real HTTP acceptance for native behaviors."""
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
from behavior_market import behavior_recipe
from microstructure_market import microstructure_recipe
from marketforge import Client


class BehaviorRecipeTests(unittest.TestCase):
    def test_export_and_room_remapping_match_and_only_pair_shares_account(self):
        self.assertEqual(behavior_recipe(), json.loads((ROOT / "scripts/fixtures/behavior_market.json").read_text(encoding="utf-8")))
        spec = behavior_recipe("remapped", 19, False)
        self.assertEqual(len(spec["agents"]), 33)
        identities = [(a["Plugin"]["participant"]["account_id"], a["Plugin"]["participant"]["instrument_id"]) for a in spec["agents"]]
        self.assertEqual(len(set(identities)), 33)
        self.assertEqual(len({a for a, _ in identities}), 32)
        for a in spec["agents"]:
            self.assertEqual(a["Plugin"]["participant"]["room_id"], "remapped")
            self.assertLessEqual(a["Plugin"]["seed"], 2**53 - 1)
        self.assertFalse(spec["autostart_agents"])
        self.assertEqual([e["id"] for e in spec["scenario"]["market_events"]], ["news-up", "news-down"])


class BehaviorHttpTests(unittest.TestCase):
    def test_microstructure_and_perp_motives_run_through_real_http_gateway(self):
        self.run_population(interval_ms=500, compressed=True, steps=60,
            receipt_name="microstructure-market-http", recipe_factory=microstructure_recipe)

    def test_live_population_receives_events_executes_pov_and_has_no_bot_errors(self):
        self.run_population(interval_ms=500, compressed=True, steps=60, receipt_name="behavior-market-http")

    def test_fast_full_timeline_has_actual_pov_fills_without_extending_order_ttl(self):
        self.run_population(interval_ms=25, compressed=False, steps=260, receipt_name="behavior-market-fast-http")

    def run_population(self, interval_ms, compressed, steps, receipt_name, recipe_factory=behavior_recipe):
        binary = Path(os.environ.get("MARKETFORGE_TEST_SERVER", str(ROOT / "target/debug/exchange-server.exe")))
        if not binary.is_file():
            self.skipTest("build an isolated server for HTTP acceptance")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        env = {k: v for k, v in os.environ.items() if not k.startswith("MARKETFORGE_")}
        env.update(MARKETFORGE_BIND_ADDR=f"127.0.0.1:{port}",
                   MARKETFORGE_AUTH_TOKENS_JSON='{"behavior-test":"behavior-owner"}',
                   PATH=str(Path(sys.executable).parent) + os.pathsep + env["PATH"])
        receipts = ROOT / ".local"
        receipts.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="marketforge-behavior-") as directory:
            with (receipts / f"{receipt_name}.log").open("w", encoding="utf-8") as log:
                process = subprocess.Popen([str(binary)], cwd=directory, env=env, stdout=log, stderr=subprocess.STDOUT)
                try:
                    client = Client(f"http://127.0.0.1:{port}", bearer="behavior-test", timeout=20)
                    deadline = time.monotonic() + 25
                    while True:
                        try:
                            client.health_ready()
                            break
                        except (RuntimeError, OSError):
                            if process.poll() is not None or time.monotonic() >= deadline:
                                self.fail("server readiness failed; see .local/behavior-market-http.log")
                            time.sleep(0.05)
                    catalog = {b["id"] for b in client.list_bots()}
                    self.assertTrue({"BasisArbitrageTrader", "MarketEventTrader", "PovExecutionTrader"} <= catalog)
                    spec = recipe_factory("http-behaviors")
                    if recipe_factory is microstructure_recipe:
                        self.assertTrue({"FundingRateTrader", "LeveragedTrendTrader"} <= catalog)
                    spec["agent_interval_ms"] = interval_ms
                    # Keep the default wall cadence. Compress only the simulation
                    # event/task timeline so acceptance does not require two minutes.
                    if compressed:
                        for event, publish, expiry in zip(spec["scenario"]["market_events"], [5000, 20000], [15000, 40000]):
                            event.update(published_at_ms=publish, expires_at_ms=expiry)
                        for agent in spec["agents"]:
                            if agent["Plugin"]["plugin_id"] == "PovExecutionTrader":
                                agent["Plugin"]["config"].update(horizon_ms=30000, deadline_urgency_ms=5000)
                    client._request("POST", "/rooms", spec)
                    deadline = time.monotonic() + 45
                    while True:
                        observation = client.observe("http-behaviors", 420)
                        observation = observation.get("observation", observation)
                        status = client._request("GET", "/rooms/http-behaviors/agents")
                        self.assertFalse(status["bot_errors"], status)
                        self.assertNotEqual(status.get("lifecycle"), "failed", status)
                        if observation["step"] >= steps:
                            break
                        if process.poll() is not None or time.monotonic() >= deadline:
                            (receipts / f"{receipt_name}-timeout.json").write_text(json.dumps({"clock_step": observation["step"], "status": status}, indent=2), encoding="utf-8")
                            self.fail(f"live behavior clock reached {observation['step']} of {steps}; see timeout receipt")
                        time.sleep(0.05)
                    client.pause_room("http-behaviors")
                    # Let outstanding native decisions commit before inspecting the saved state.
                    deadline = time.monotonic() + 5
                    while True:
                        saved = client.room_bots("http-behaviors")
                        states = {a["template"]["Plugin"]["participant"]["participant_id"]: a["kind_state"]["Plugin"]["data"] for a in saved["agents"]}
                        if all(len(states[f"event-{i}"]["received_event_ids"]) == 2 for i in range(4)):
                            break
                        if time.monotonic() >= deadline:
                            self.fail(f"events were not received: {states}")
                        time.sleep(0.05)
                    (receipts / f"{receipt_name}-last-state.json").write_text(json.dumps({"clock_step": observation["step"], "status": status, "states": states}, ensure_ascii=False, indent=2), encoding="utf-8")
                    self.assertEqual(len(saved["agents"]), len(spec["agents"]))
                    for name in ["pov-buy", "pov-sell"]:
                        self.assertGreater(states[name]["completed_qty"], 0)
                        self.assertLessEqual(states[name]["completed_qty"], 25)
                        self.assertTrue(states[name]["deadline_reached"])
                    self.assertEqual([e["id"] for e in observation["market_events"]], ["news-up", "news-down"])
                    self.assertTrue(observation["public_trades"])
                    self.assertTrue(all(t["maker_account_id"] != t["taker_account_id"] for t in observation["public_trades"]))
                    (receipts / f"{receipt_name}.json").write_text(json.dumps({
                        "clock_step": observation["step"], "status": status, "states": states,
                        "public_trades": observation["public_trades"], "events": observation["market_events"],
                    }, ensure_ascii=False, indent=2), encoding="utf-8")
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
