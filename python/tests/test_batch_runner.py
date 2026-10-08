#!/usr/bin/env python3
"""F6 batch-runner behavior tests. Offline units plus live HTTP/Postgres."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PYTHON_ROOT = ROOT / "python"
SCRIPTS = ROOT / "scripts"
sys.path.insert(0, str(PYTHON_ROOT))
sys.path.insert(0, str(SCRIPTS))

from marketforge import Client, MarketForgeError  # noqa: E402
from marketforge.batch import (  # noqa: E402
    StateStore,
    child_seed,
    experiment_identity,
    inject_seed,
    is_terminal_row,
    is_terminal_status,
    prepare_request,
    run_batch,
    start_or_lookup,
    summarize,
    TERMINAL_STATUSES,
)

DSN = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
REQUIRE_PG = os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1"
BIND = os.environ.get("MARKETFORGE_F6_BIND", "127.0.0.1:57531")
AUTH_JSON = '{"admin-token":"admin"}'
BEARER = "admin-token"


def _spot_scenario(room_id: str, sell_qty: int = 8) -> dict:
    return {
        "room_id": room_id,
        "market": {
            "Spot": {
                "instrument": {
                    "instrument_id": "V-BTC-SPOT",
                    "venue_id": "default-venue",
                    "symbol": "V-BTC-SPOT",
                    "base_asset": "V",
                    "quote_asset": "BTC",
                    "tick_size": 1,
                    "lot_size": 1,
                },
                "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0},
                "risk": {
                    "price_tick_size": None,
                    "lot_size": None,
                    "max_order_qty": None,
                    "max_order_notional": None,
                    "allow_short": True,
                },
            }
        },
        "accounts": [
            {"Spot": {"account_id": 10, "cash_balance": 100000, "position_qty": 50}},
            {"Spot": {"account_id": 20, "cash_balance": 100000, "position_qty": 0}},
        ],
        "seed_orders": [
            {
                "NewOrder": {
                    "order_id": 1,
                    "account_id": 10,
                    "side": "Buy",
                    "kind": {"Limit": {"price_tick": 99}},
                    "qty": sell_qty,
                    "reduce_only": False,
                }
            },
            {
                "NewOrder": {
                    "order_id": 2,
                    "account_id": 10,
                    "side": "Sell",
                    "kind": {"Limit": {"price_tick": 101}},
                    "qty": sell_qty,
                    "reduce_only": False,
                }
            },
        ],
    }


def _noise_agent(room_id: str, seed: int = 1) -> dict:
    return {
        "NoiseTrader": {
            "participant": {
                "participant_id": "noise",
                "kind": "RuleAgent",
                "room_id": room_id,
                "account_id": 10,
                "instrument_id": "V-BTC-SPOT",
            },
            "seed": seed,
            "reference_price_tick": 100,
            "price_radius_ticks": 3,
            "max_qty": 2,
            "market_order_ratio_ppm": 0,
        }
    }


def make_spec(*, room: str = "f6", agents: bool = False, target_qty: int = 1, horizon: int = 4) -> dict:
    spec = {
        "run_id": room,
        "scenario": _spot_scenario(room),
        "agents": [_noise_agent(room)] if agents else [],
        "trainee_account_id": 20,
        "target_qty": target_qty,
        "horizon_steps": horizon,
    }
    return spec


class ChildSeedAndIdentityTests(unittest.TestCase):
    def test_child_seed_matches_rust_derivation(self) -> None:
        self.assertEqual(child_seed(7, "mm"), 109053961290706883)
        self.assertEqual(child_seed(7, "flow"), 5553730247978922617)
        self.assertEqual(child_seed(8, "mm"), 124359163149538028)
        self.assertNotEqual(child_seed(7, "mm"), child_seed(7, "flow"))
        self.assertNotEqual(child_seed(7, "mm"), child_seed(8, "mm"))
        self.assertEqual(child_seed(7, "mm"), child_seed(7, "mm"))

    def test_inject_seed_does_not_mutate_target_qty(self) -> None:
        spec = make_spec(agents=True, target_qty=4)
        original_qty = spec["target_qty"]
        original_agent_seed = spec["agents"][0]["NoiseTrader"]["seed"]
        injected = inject_seed(spec, 7)
        self.assertEqual(spec["target_qty"], original_qty)
        self.assertEqual(spec["agents"][0]["NoiseTrader"]["seed"], original_agent_seed)
        self.assertEqual(injected["target_qty"], 4)
        self.assertEqual(injected["agents"][0]["NoiseTrader"]["seed"], child_seed(7, "noise"))
        self.assertNotEqual(injected["agents"][0]["NoiseTrader"]["seed"], original_agent_seed)

    def test_name_only_difference_is_not_a_different_experiment(self) -> None:
        spec_a = make_spec(room="room-a", agents=True)
        spec_b = make_spec(room="room-b", agents=True)
        spec_b["run_id"] = "other-name"
        self.assertEqual(experiment_identity(spec_a, 3)["digest"], experiment_identity(spec_b, 3)["digest"])
        self.assertNotEqual(experiment_identity(spec_a, 3)["digest"], experiment_identity(spec_a, 4)["digest"])
        spec_b["target_qty"] = 9
        self.assertNotEqual(experiment_identity(spec_a, 3)["digest"], experiment_identity(spec_b, 3)["digest"])

    def test_prepare_request_isolates_rooms(self) -> None:
        spec = make_spec(room="iso", agents=True)
        one = prepare_request(spec, 1)
        two = prepare_request(spec, 2)
        self.assertNotEqual(one["run_id"], two["run_id"])
        self.assertNotEqual(one["scenario"]["room_id"], two["scenario"]["room_id"])
        self.assertEqual(one["run_id"], one["scenario"]["room_id"])
        self.assertEqual(one["agents"][0]["NoiseTrader"]["participant"]["room_id"], one["scenario"]["room_id"])
        self.assertEqual(one["target_qty"], spec["target_qty"])
        prefixed = prepare_request(spec, 1, run_prefix="p-a")
        other = prepare_request(spec, 1, run_prefix="p-b")
        self.assertNotEqual(prefixed["scenario"]["room_id"], other["scenario"]["room_id"])
        self.assertTrue(prefixed["run_id"].startswith("p-a-"))
        self.assertEqual(prefixed["run_id"], prefixed["scenario"]["room_id"])

    def test_lost_start_looks_up_existing_run(self) -> None:
        class FakeClient:
            def __init__(self) -> None:
                self.starts = 0

            def start_training(self, request: dict) -> dict:
                self.starts += 1
                raise urllib.error.URLError("lost response")

            def training_status(self, run_id: str) -> dict:
                return {"run": {"status": "Running", "spec": {"run_id": run_id}}, "score": {"finished": False}}

        fake = FakeClient()
        payload = start_or_lookup(fake, {"run_id": "kept-run", "scenario": {"room_id": "kept"}})
        self.assertEqual(payload["run"]["spec"]["run_id"], "kept-run")
        self.assertEqual(fake.starts, 1)


class StateAndSummaryTests(unittest.TestCase):
    def test_atomic_state_replace_and_corrupt_load(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state.json"
            store = StateStore(path)
            store.acquire()
            try:
                store.upsert({"run_id": "a", "seed": 1, "ok": True, "phase": "completed", "status": "Completed"})
                text = path.read_text()
                self.assertTrue(text.endswith("\n"))
                loaded = store.load()
                self.assertEqual(len(loaded["runs"]), 1)
                path.write_text("{not json")
                corrupt = store.load()
                self.assertEqual(corrupt["runs"], [])
                self.assertTrue(corrupt.get("corrupt"))
                store.upsert({"run_id": "b", "seed": 2, "ok": False, "phase": "failed", "status": "Failed"})
                recovered = json.loads(path.read_text())
                self.assertEqual(len(recovered["runs"]), 1)
                self.assertEqual(recovered["runs"][0]["run_id"], "b")
            finally:
                store.release()

    def test_concurrent_same_store_writes_stay_valid_json(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state.json"
            store = StateStore(path)
            store.acquire()
            try:
                errors: list[BaseException] = []

                def writer(seed: int) -> None:
                    try:
                        store.upsert({"run_id": f"r-{seed}", "seed": seed, "ok": True, "phase": "completed"})
                    except BaseException as exc:  # noqa: BLE001
                        errors.append(exc)

                threads = [threading.Thread(target=writer, args=(i,)) for i in range(8)]
                for thread in threads:
                    thread.start()
                for thread in threads:
                    thread.join()
                self.assertEqual(errors, [])
                data = json.loads(path.read_text())
                self.assertEqual(len(data["runs"]), 8)
            finally:
                store.release()

    def test_summary_keeps_failures_and_rejects_zero_fill_wins(self) -> None:
        rows = [
            {
                "seed": 1,
                "ok": True,
                "status": "Completed",
                "score": {"q": 4, "buy_slippage_bp": 10, "fees_paid": 1, "incomplete": False},
            },
            {
                "seed": 2,
                "ok": False,
                "status": "Aborted",
                "failure_type": "injected_fail_seed",
                "score": {"q": 0, "buy_slippage_bp": None, "fees_paid": 0, "incomplete": True},
            },
            {
                "seed": 3,
                "ok": True,
                "status": "Completed",
                "score": {"q": 0, "buy_slippage_bp": -999, "fees_paid": 0, "incomplete": True},
            },
            {
                "seed": 4,
                "ok": False,
                "status": "Failed",
                "score": None,
                "failure_type": "http",
            },
        ]
        summary = summarize(rows)
        self.assertEqual(summary["sample_size"], 4)
        self.assertEqual(len(summary["runs"]), 4)
        self.assertEqual(len(summary["failures"]), 2)
        self.assertTrue(summary["zero_fills_not_win"])
        zero = next(row for row in summary["runs"] if row["seed"] == 3)
        self.assertTrue(zero["zero_fills"])
        self.assertIs(zero["low_cost_win"], False)
        missing = next(row for row in summary["runs"] if row["seed"] == 4)
        self.assertTrue(missing["metric_missing"])
        self.assertIn(None, summary["costs"])

    def test_running_row_is_not_terminal(self) -> None:
        self.assertFalse(is_terminal_row({"status": "Running", "ok": True}))
        self.assertFalse(is_terminal_status("Running"))
        self.assertTrue(is_terminal_status("Completed"))
        self.assertEqual(TERMINAL_STATUSES, frozenset({"Completed", "Failed", "Aborted"}))


@unittest.skipUnless(REQUIRE_PG or bool(DSN), "MARKETFORGE_TEST_DATABASE_URL not set")
class LiveBatchTests(unittest.TestCase):
    server: subprocess.Popen | None = None
    log_path = ROOT / "target" / "f6-batch-server.log"
    base_url = f"http://{BIND}"
    prefix = f"f6-{int(time.time())}-{os.getpid()}"

    @classmethod
    def setUpClass(cls) -> None:
        if REQUIRE_PG and not DSN:
            raise AssertionError("MARKETFORGE_REQUIRE_POSTGRES_TESTS=1 but MARKETFORGE_TEST_DATABASE_URL is empty")
        if not DSN:
            raise unittest.SkipTest("MARKETFORGE_TEST_DATABASE_URL not set")
        binary = Path(os.environ.get("MARKETFORGE_TEST_SERVER", str(
            ROOT / "target/debug" / ("exchange-server.exe" if os.name == "nt" else "exchange-server"))))
        if not binary.exists():
            built = subprocess.run(
                ["cargo", "build", "-p", "exchange-server"],
                cwd=ROOT,
                check=False,
                capture_output=True,
                text=True,
            )
            if built.returncode != 0:
                raise AssertionError(built.stderr[-4000:])
        cls.log_path.parent.mkdir(parents=True, exist_ok=True)
        env = os.environ.copy()
        env["MARKETFORGE_DATABASE_URL"] = DSN
        env["MARKETFORGE_AUTH_TOKENS_JSON"] = AUTH_JSON
        env["MARKETFORGE_BIND_ADDR"] = BIND
        env["MARKETFORGE_INSTANCE_ID"] = f"f6-{os.getpid()}"
        env["MARKETFORGE_ADVERTISE_URL"] = cls.base_url
        env["MARKETFORGE_RUNTIME_MODE"] = "single-active"
        env["MARKETFORGE_RUNTIME_LOCK_WAIT_MS"] = "0"
        log = open(cls.log_path, "w", encoding="utf-8")
        cls.server = subprocess.Popen(
            [str(binary)],
            cwd=ROOT,
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        cls._log_handle = log
        deadline = time.monotonic() + 20
        last_error = None
        while time.monotonic() < deadline:
            if cls.server.poll() is not None:
                raise AssertionError(f"exchange-server exited {cls.server.returncode}: {cls.log_path.read_text()[-4000:]}")
            try:
                Client(cls.base_url, bearer=BEARER, trusted_owner_urls=[cls.base_url]).health_ready()
                return
            except Exception as exc:  # noqa: BLE001
                last_error = exc
                time.sleep(0.2)
        raise AssertionError(f"server not ready: {last_error}\n{cls.log_path.read_text()[-4000:]}")

    @classmethod
    def tearDownClass(cls) -> None:
        if cls.server is not None and cls.server.poll() is None:
            cls.server.terminate()
            try:
                cls.server.wait(timeout=8)
            except subprocess.TimeoutExpired:
                cls.server.kill()
        handle = getattr(cls, "_log_handle", None)
        if handle is not None:
            handle.close()

    def client(self) -> Client:
        return Client(self.base_url, bearer=BEARER, trusted_owner_urls=[self.base_url], timeout=30)

    def test_start_is_not_treated_as_completed_score(self) -> None:
        spec = make_spec(room=f"{self.prefix}-start", agents=False, target_qty=4, horizon=8)
        request = prepare_request(spec, 1, run_prefix=f"{self.prefix}-start")
        payload = start_or_lookup(self.client(), request)
        self.assertEqual(payload["run"]["status"], "Running")
        self.assertFalse(payload["score"]["finished"])
        self.assertNotIn(payload["run"]["status"], TERMINAL_STATUSES)

    def test_seed_changes_agent_rng_on_server(self) -> None:
        spec = make_spec(room=f"{self.prefix}-rng", agents=True, target_qty=1, horizon=6)
        client = self.client()
        summary = run_batch(
            client,
            spec,
            [1, 2],
            concurrency=2,
            timeout_seconds=30,
            run_prefix=f"{self.prefix}-rng",
        )
        self.assertEqual(summary["sample_size"], 2)
        rows = {row["seed"]: row for row in summary["runs"]}
        status_one = client.training_status(rows[1]["run_id"])
        status_two = client.training_status(rows[2]["run_id"])
        seed_one = status_one["run"]["spec"]["agents"][0]["NoiseTrader"]["seed"]
        seed_two = status_two["run"]["spec"]["agents"][0]["NoiseTrader"]["seed"]
        self.assertEqual(seed_one, child_seed(1, "noise"))
        self.assertEqual(seed_two, child_seed(2, "noise"))
        self.assertNotEqual(seed_one, seed_two)
        events_one = client.events(rows[1]["room_id"], limit=50, from_start=True)
        events_two = client.events(rows[2]["room_id"], limit=50, from_start=True)
        self.assertNotEqual(
            json.dumps(events_one.get("executions"), sort_keys=True, default=str),
            json.dumps(events_two.get("executions"), sort_keys=True, default=str),
        )

    def test_crash_mid_run_resumes_same_server_run(self) -> None:
        spec = make_spec(room=f"{self.prefix}-crash", agents=False, target_qty=40, horizon=5)
        with tempfile.TemporaryDirectory() as tmp:
            state_path = Path(tmp) / "state.json"
            first = run_batch(
                self.client(),
                spec,
                [1],
                state_path=state_path,
                timeout_seconds=30,
                max_clock_advances=1,
                drive_strategy_enabled=False,
                run_prefix=f"{self.prefix}-crash",
            )
            self.assertEqual(len(first["runs"]), 1)
            row = first["runs"][0]
            self.assertEqual(row["run_id"], prepare_request(spec, 1, run_prefix=f"{self.prefix}-crash")["run_id"])
            self.assertFalse(is_terminal_status(row.get("status") or ""))
            self.assertEqual(row["phase"], "running")
            live = self.client().training_status(row["run_id"])
            self.assertEqual(live["run"]["status"], "Running")
            second = run_batch(
                self.client(),
                spec,
                [1],
                state_path=state_path,
                timeout_seconds=30,
                drive_strategy_enabled=False,
                run_prefix=f"{self.prefix}-crash",
            )
            resumed = second["runs"][0]
            self.assertEqual(resumed["run_id"], row["run_id"])
            self.assertTrue(is_terminal_status(resumed["status"]))
            self.assertEqual(resumed["server_run_id"], row["run_id"])
            authority = self.client().training_result(resumed["run_id"])
            self.assertEqual(authority["run"]["status"], resumed["status"])
            self.assertEqual(authority["score"]["q"], (resumed.get("score") or {}).get("q"))

    def test_same_run_retry_returns_original(self) -> None:
        spec = make_spec(room=f"{self.prefix}-retry", agents=False, target_qty=1, horizon=4)
        with tempfile.TemporaryDirectory() as tmp:
            state_path = Path(tmp) / "state.json"
            first = run_batch(
                self.client(),
                spec,
                [1],
                state_path=state_path,
                timeout_seconds=30,
                run_prefix=f"{self.prefix}-retry",
            )
            run_id = first["runs"][0]["run_id"]
            self.assertTrue(first["runs"][0]["ok"])
            second = run_batch(
                self.client(),
                spec,
                [1],
                state_path=state_path,
                timeout_seconds=30,
                run_prefix=f"{self.prefix}-retry",
            )
            self.assertTrue(second["runs"][0].get("resumed"))
            self.assertEqual(second["runs"][0]["run_id"], run_id)
            again = start_or_lookup(self.client(), prepare_request(spec, 1, run_prefix=f"{self.prefix}-retry"))
            self.assertEqual(again["run"]["spec"]["run_id"], run_id)

    def test_bounded_concurrency(self) -> None:
        spec = make_spec(room=f"{self.prefix}-conc", agents=False, target_qty=1, horizon=4)
        counter = [0, 0]
        lock = threading.Lock()
        summary = run_batch(
            self.client(),
            spec,
            [1, 2, 3],
            concurrency=2,
            timeout_seconds=40,
            run_prefix=f"{self.prefix}-conc",
            active_counter=counter,
            active_lock=lock,
        )
        self.assertEqual(summary["sample_size"], 3)
        self.assertLessEqual(counter[1], 2)
        self.assertGreaterEqual(counter[1], 2)
        for row in summary["runs"]:
            self.assertTrue(is_terminal_status(row["status"]), row)

    def test_cancel_one_run_leaves_others(self) -> None:
        spec = make_spec(room=f"{self.prefix}-cancel", agents=False, target_qty=1, horizon=6)
        cancel_id = prepare_request(spec, 2, run_prefix=f"{self.prefix}-cancel")["run_id"]
        summary = run_batch(
            self.client(),
            spec,
            [1, 2],
            concurrency=2,
            timeout_seconds=30,
            cancel_run_ids={cancel_id},
            run_prefix=f"{self.prefix}-cancel",
        )
        by_seed = {row["seed"]: row for row in summary["runs"]}
        self.assertTrue(by_seed[1]["ok"])
        self.assertEqual(by_seed[1]["status"], "Completed")
        self.assertFalse(by_seed[2]["ok"])
        self.assertEqual(by_seed[2]["status"], "Aborted")
        self.assertEqual(by_seed[2]["run_id"], cancel_id)
        other = self.client().training_status(by_seed[1]["run_id"])
        self.assertEqual(other["run"]["status"], "Completed")

    def test_failed_runs_remain_and_zero_fills_are_not_wins(self) -> None:
        spec = make_spec(room=f"{self.prefix}-fail", agents=False, target_qty=4, horizon=3)
        summary = run_batch(
            self.client(),
            spec,
            [1, 99],
            fail_seeds=[99],
            timeout_seconds=30,
            drive_strategy_enabled=False,
            run_prefix=f"{self.prefix}-fail",
        )
        self.assertEqual(len(summary["runs"]), 2)
        self.assertEqual(len(summary["failures"]), 1)
        by_seed = {row["seed"]: row for row in summary["runs"]}
        self.assertEqual(by_seed[99]["failure_type"], "injected_fail_seed")
        self.assertFalse(by_seed[99]["ok"])
        self.assertEqual(by_seed[1]["status"], "Completed")
        self.assertTrue(by_seed[1].get("zero_fills") or (by_seed[1].get("score") or {}).get("q") == 0)
        self.assertIs(by_seed[1].get("low_cost_win"), False)
        self.assertTrue(summary["zero_fills_not_win"])
        live = self.client().training_status(by_seed[99]["run_id"])
        self.assertEqual(live["run"]["status"], "Aborted")

    def test_cli_batch_runner_emits_terminal_rows(self) -> None:
        spec_path = SCRIPTS / "fixtures" / "f6_batch_spec.json"
        prefix = f"{self.prefix}-cli"
        with tempfile.TemporaryDirectory() as tmp:
            state_path = Path(tmp) / "state.json"
            env = os.environ.copy()
            env["MARKETFORGE_BEARER"] = BEARER
            proc = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "batch_runner.py"),
                    self.base_url,
                    str(spec_path),
                    "1",
                    "--state",
                    str(state_path),
                    "--bearer",
                    BEARER,
                    "--run-prefix",
                    prefix,
                    "--timeout-seconds",
                    "30",
                    "--concurrency",
                    "1",
                ],
                cwd=ROOT,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
            if proc.returncode != 0:
                self.fail(f"cli failed {proc.returncode}: {proc.stdout}\n{proc.stderr}")
            summary = json.loads(proc.stdout)
            self.assertEqual(len(summary["runs"]), 1)
            row = summary["runs"][0]
            self.assertTrue(is_terminal_status(row["status"]), row)
            self.assertTrue(row["ok"], row)
            live = self.client().training_result(row["run_id"])
            self.assertEqual(live["run"]["spec"]["run_id"], row["run_id"])
            self.assertTrue(live["score"]["finished"])


if __name__ == "__main__":
    unittest.main()
