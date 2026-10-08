"""Plugin identity units and real HTTP/PostgreSQL restart and batch acceptance."""
from __future__ import annotations

import copy
import json
import os
import socket
import subprocess
import sys
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "python"))
from marketforge import Client, MarketForgeError
from marketforge.batch import child_seed, experiment_identity, prepare_request, run_batch


def plugin_spec(name: str = "plugin", qty: int = 2) -> dict:
    spec = json.loads((ROOT / "scripts/fixtures/f6_batch_spec.json").read_text())
    spec["run_id"] = name
    spec["scenario"]["room_id"] = name
    spec["target_qty"] = qty
    spec["horizon_steps"] = 8
    spec["agents"] = [{"Plugin": {
        "participant": {"participant_id": "buyer", "kind": "RuleAgent", "room_id": name,
                        "account_id": 20, "instrument_id": "V-BTC-SPOT"},
        "plugin_id": "example.buy-remaining", "plugin_version": "1.0.0", "state_version": 1,
        "seed": 1, "config": {"target_qty": qty, "qty_per_step": 1},
    }}]
    return spec


class PluginIdentityTests(unittest.TestCase):
    def test_seed_version_params_and_room_isolation(self):
        spec = plugin_spec()
        request = prepare_request(spec, 7)
        self.assertTrue(request["manual_agents"])
        self.assertEqual(request["agents"][0]["Plugin"]["seed"], child_seed(7, "buyer"))
        self.assertEqual(spec["agents"][0]["Plugin"]["seed"], 1)
        without_seed = copy.deepcopy(spec)
        del without_seed["agents"][0]["Plugin"]["seed"]
        self.assertEqual(prepare_request(without_seed, 7)["agents"][0]["Plugin"]["seed"], child_seed(7, "buyer"))
        self.assertEqual(request["agents"][0]["Plugin"]["participant"]["room_id"], request["scenario"]["room_id"])
        identity = experiment_identity(spec, 7)
        self.assertEqual(identity["strategy_version"], "1.0.0")
        renamed = plugin_spec("different-name")
        self.assertEqual(experiment_identity(renamed, 7)["digest"], identity["digest"])
        for field, value in [("plugin_version", "2.0.0"), ("state_version", 2), ("config", {"target_qty": 3})]:
            changed = copy.deepcopy(spec)
            changed["agents"][0]["Plugin"][field] = value
            self.assertNotEqual(experiment_identity(changed, 7)["digest"], identity["digest"])

    def test_plugin_evaluation_requires_explicit_trainee_bot(self):
        spec = plugin_spec()
        spec["agents"][0]["Plugin"]["participant"]["account_id"] = 10
        with self.assertRaisesRegex(ValueError, "trainee_account_id"):
            prepare_request(spec, 1)


class PluginLiveTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not cls.dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                raise AssertionError("forced PostgreSQL plugin tests require a database URL")
            raise unittest.SkipTest("MARKETFORGE_TEST_DATABASE_URL not set")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            cls.port = sock.getsockname()[1]
        cls.base = f"http://127.0.0.1:{cls.port}"
        cls.prefix = f"bot-e2e-{os.getpid()}-{time.time_ns()}"
        cls.logs = ROOT / "target/bot-plugin-e2e"
        cls.logs.mkdir(parents=True, exist_ok=True)
        cls.server = None
        cls.log = None
        cls.env = os.environ.copy()
        cls.env.update({
            "MARKETFORGE_DATABASE_URL": cls.dsn,
            "MARKETFORGE_AUTH_TOKENS_JSON": '{"plugin-admin":"admin"}',
            "MARKETFORGE_BIND_ADDR": f"127.0.0.1:{cls.port}",
            "MARKETFORGE_BOT_PLUGIN_DIR": str(ROOT / "bot-plugins"),
            "MARKETFORGE_RUNTIME_MODE": "single-active",
            "MARKETFORGE_RUNTIME_LOCK_WAIT_MS": "3000",
        })
        for key in ("MARKETFORGE_INSTANCE_ID", "MARKETFORGE_ADVERTISE_URL"):
            cls.env.pop(key, None)
        cls.start_server()

    @classmethod
    def start_server(cls):
        cls.log = open(cls.logs / f"{cls.prefix}-{time.time_ns()}.log", "w")
        cls.server = subprocess.Popen([str(ROOT / "target/debug/exchange-server")], cwd=ROOT,
                                     env=cls.env, stdout=cls.log, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if cls.server.poll() is not None:
                cls.log.close()
                raise AssertionError(f"server exited {cls.server.returncode}; see {cls.logs}")
            try:
                cls.client().health_ready()
                return
            except (MarketForgeError, OSError):
                time.sleep(0.05)
        cls.stop_server()
        raise AssertionError(f"server not ready; see {cls.logs}")

    @classmethod
    def stop_server(cls):
        if cls.server is not None and cls.server.poll() is None:
            cls.server.terminate()
            try:
                cls.server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                cls.server.kill()
                cls.server.wait(timeout=5)
        if cls.log is not None:
            cls.log.close()

    @classmethod
    def tearDownClass(cls):
        cls.stop_server()

    @classmethod
    def client(cls):
        return Client(cls.base, bearer="plugin-admin", trusted_owner_urls=[cls.base], timeout=15)

    def test_catalog_and_invalid_configuration(self):
        client = self.client()
        bots = client.list_bots()
        self.assertEqual(len(bots), 6)
        self.assertIn("example.buy-remaining", {bot["id"] for bot in bots})
        with self.assertRaises(MarketForgeError) as error:
            Client(self.base).list_bots()
        self.assertEqual(error.exception.status, 401)
        spec = plugin_spec(self.prefix + "-bad")
        spec["agents"][0]["Plugin"]["config"]["target_qty"] = 0
        with self.assertRaises(MarketForgeError) as error:
            client.start_training(spec)
        self.assertEqual(error.exception.status, 400)

    def test_postgres_restart_recovers_state_and_step_idempotency(self):
        client = self.client()
        spec = prepare_request(plugin_spec(self.prefix + "-restart"), 7)
        run_id, room_id = spec["run_id"], spec["scenario"]["room_id"]
        self.assertEqual(client.start_training(spec)["run"]["filled_qty"], 0)
        client.pause_room(room_id)
        # Initial configuration was persisted before any step.
        self.assertEqual(client.room_bots(room_id)["agents"][0]["kind_state"]["Plugin"]["data"], None)
        first = client.step_bots(room_id, "restart-step-0")
        before = client.training_status(run_id)
        self.assertEqual(before["run"]["filled_qty"], 1)
        self.assertEqual(before["run"]["steps_elapsed"], 1)
        self.stop_server()
        self.start_server()
        client = self.client()
        self.assertEqual(client.room_bots(room_id), first)
        self.assertEqual(client.step_bots(room_id, "restart-step-0"), first)
        self.assertEqual(client.training_status(run_id), before)
        client.step_bots(room_id, "restart-step-1")
        final = client.training_status(run_id)
        self.assertEqual(final["run"]["status"], "Completed")
        self.assertEqual(final["run"]["filled_qty"], 2)
        self.assertEqual(len(final["run"]["fills"]), 2)
        self.assertEqual(client.room_bots(room_id)["agents"][0]["kind_state"]["Plugin"]["data"]["observed_steps"], 2)

    def test_batch_evaluates_plugin_without_default_buyer(self):
        summary = run_batch(self.client(), plugin_spec(self.prefix + "-batch"), [3, 5], concurrency=2,
                            timeout_seconds=20, poll_interval=0.01)
        self.assertEqual(summary["completed"], 2)
        self.assertEqual(summary["failures"], [])
        for row in summary["runs"]:
            run = self.client().training_status(row["run_id"])["run"]
            self.assertEqual(run["filled_qty"], 2)
            self.assertEqual(len(run["fills"]), 2)
            state = self.client().room_bots(row["room_id"])
            self.assertEqual(state["agents"][0]["kind_state"]["Plugin"]["data"]["observed_steps"], 2)
            self.assertEqual(run["steps_elapsed"], 1)  # Target completes on the second fill, before on_step.

    def test_start_stop_and_adding_instance_preserves_existing_state(self):
        client = self.client()
        spec = plugin_spec(self.prefix + "-instances")
        room_id = spec["scenario"]["room_id"]
        client._request("POST", "/rooms", {"scenario": spec["scenario"]})
        client.pause_room(room_id)
        first_bot = spec["agents"][0]
        self.assertTrue(client.start_agents(room_id, [first_bot], 1000)["running"])
        client.step_bots(room_id, "instance-step-0")
        first_state = client.room_bots(room_id)["agents"][0]["kind_state"]
        self.assertFalse(client.stop_agents(room_id)["running"])
        second_bot = copy.deepcopy(first_bot)
        second_bot["Plugin"]["participant"]["participant_id"] = "buyer-2"
        status = client.start_agents(room_id, [first_bot, second_bot], 1000)
        self.assertEqual(status["participants"], ["buyer", "buyer-2"])
        self.assertEqual(client.room_bots(room_id)["agents"][0]["kind_state"], first_state)
        client.stop_agents(room_id)


if __name__ == "__main__":
    unittest.main()
