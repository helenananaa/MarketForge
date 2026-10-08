"""Headless batch evaluation for training runs.

Start success is Started/Running only. The runner drives strategy and clock until a
terminal status, then stores the server score. Seed is injected into agent RNG via
the same child-seed derivation as exchange-core (`child_seed`). Local JSON is not
the score authority.
"""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import sys
import tempfile
import threading
import time
import urllib.error
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any, Callable, Iterable, Optional

from marketforge import Client, MarketForgeError

TERMINAL_STATUSES = frozenset({"Completed", "Failed", "Aborted"})
CHILD_SEED_PRIME = 0x1000_0000_01b3
MASK64 = (1 << 64) - 1
DEFAULT_SCORING_VERSION = 1

Hook = Optional[Callable[[dict[str, Any]], None]]


def child_seed(scenario_seed: int, name: str) -> int:
    """Match `exchange_core::training_scenarios::child_seed` (wrapping u64 FNV-like)."""
    h = max(int(scenario_seed), 1)
    for byte in name.encode("utf-8"):
        h = (h * CHILD_SEED_PRIME) & MASK64
        h = (h + byte) & MASK64
    return max(h, 1)


def canonical_json(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), default=str)


def agent_kind_and_body(agent: Any) -> tuple[Optional[str], Optional[dict[str, Any]]]:
    if not isinstance(agent, dict) or not agent:
        return None, None
    kind, body = next(iter(agent.items()))
    if isinstance(body, dict):
        return str(kind), body
    return str(kind), None


def agent_name(agent: Any) -> str:
    kind, body = agent_kind_and_body(agent)
    if body is not None:
        participant = body.get("participant") or {}
        if isinstance(participant, dict) and participant.get("participant_id"):
            return str(participant["participant_id"])
    return kind or "agent"


def has_plugin_agents(request: dict[str, Any]) -> bool:
    return any(agent_kind_and_body(agent)[0] == "Plugin" for agent in request.get("agents") or [])


def has_trainee_agent(request: dict[str, Any]) -> bool:
    account_id = request.get("trainee_account_id")
    return any(
        isinstance(body, dict) and (body.get("participant") or {}).get("account_id") == account_id
        for _kind, body in (agent_kind_and_body(agent) for agent in request.get("agents") or [])
    )


def inject_seed(spec: dict[str, Any], seed: int) -> dict[str, Any]:
    """Copy spec and set each agent `seed` field from child_seed(parent, agent name).

    Does not mutate target_qty, horizon, or accounts. Name-only room/run changes are
    not a different experiment; identity uses the injected seeds plus params.
    """
    request = copy.deepcopy(spec)
    for agent in request.get("agents") or []:
        kind, body = agent_kind_and_body(agent)
        if body is not None and ("seed" in body or kind == "Plugin"):
            body["seed"] = child_seed(seed, agent_name(agent))
    return request


def _rewrite_agent_rooms(request: dict[str, Any], room_id: str) -> None:
    for agent in request.get("agents") or []:
        _kind, body = agent_kind_and_body(agent)
        if body is None:
            continue
        participant = body.get("participant")
        if isinstance(participant, dict):
            participant["room_id"] = room_id


def experiment_identity(spec: dict[str, Any], seed: int) -> dict[str, Any]:
    injected = inject_seed(spec, seed)
    scenario = copy.deepcopy(injected.get("scenario") or {})
    scenario.pop("room_id", None)
    agents = copy.deepcopy(injected.get("agents") or [])
    for agent in agents:
        _kind, body = agent_kind_and_body(agent)
        if body is not None and isinstance(body.get("participant"), dict):
            body["participant"].pop("room_id", None)
    strategy_version = 1
    for agent in agents:
        _kind, body = agent_kind_and_body(agent)
        if body is not None and body.get("plugin_version") is not None:
            strategy_version = body["plugin_version"]
            break
        if body is not None and body.get("version") is not None:
            strategy_version = body["version"]
            break
    payload = {
        "scenario": scenario,
        "agents": agents,
        "target_qty": injected.get("target_qty"),
        "horizon_steps": injected.get("horizon_steps"),
        "trainee_account_id": injected.get("trainee_account_id"),
        "seed": int(seed),
        "scoring_version": injected.get("scoring_version", DEFAULT_SCORING_VERSION),
        "task_version": injected.get("task_version", 1),
        "spec_version": injected.get("spec_version", 1),
        "strategy_version": strategy_version,
    }
    digest = hashlib.sha256(canonical_json(payload).encode("utf-8")).hexdigest()
    return {
        "digest": digest,
        "seed": int(seed),
        "scoring_version": payload["scoring_version"],
        "scenario_digest": hashlib.sha256(canonical_json(scenario).encode("utf-8")).hexdigest(),
        "strategy_version": strategy_version,
        "params": {
            "target_qty": payload["target_qty"],
            "horizon_steps": payload["horizon_steps"],
            "trainee_account_id": payload["trainee_account_id"],
        },
    }


def prepare_request(
    spec: dict[str, Any],
    seed: int,
    *,
    run_prefix: Optional[str] = None,
    max_steps: Optional[int] = None,
) -> dict[str, Any]:
    request = inject_seed(spec, seed)
    identity = experiment_identity(spec, seed)
    base = run_prefix or spec.get("run_id") or (spec.get("scenario") or {}).get("room_id") or "batch"
    suffix = f"{seed}-{identity['digest'][:12]}"
    request["run_id"] = f"{base}-{suffix}"
    if "scenario" not in request or not isinstance(request["scenario"], dict):
        request["scenario"] = {}
    request["scenario"]["room_id"] = f"{base}-{suffix}"
    _rewrite_agent_rooms(request, request["scenario"]["room_id"])
    if has_plugin_agents(request):
        if not has_trainee_agent(request):
            raise ValueError("plugin evaluation requires a bot bound to trainee_account_id")
        request["manual_agents"] = True
    if max_steps is not None:
        request["horizon_steps"] = max_steps
    return request


def run_status(payload: Any) -> str:
    if not isinstance(payload, dict):
        return ""
    run = payload.get("run") if isinstance(payload.get("run"), dict) else payload
    return str(run.get("status") or "")


def is_terminal_status(status: str) -> bool:
    return status in TERMINAL_STATUSES


def is_terminal_row(row: dict[str, Any]) -> bool:
    status = str(row.get("status") or "")
    phase = str(row.get("phase") or "")
    if phase in {"completed", "failed"}:
        return True
    return is_terminal_status(status)


def start_or_lookup(client: Client, request: dict[str, Any]) -> dict[str, Any]:
    """Start a run; on lost response or existing room, look up the same run_id."""
    try:
        return client.start_training(request)
    except MarketForgeError as exc:
        existing = _try_status(client, request["run_id"])
        if existing is not None:
            return existing
        raise
    except (TimeoutError, urllib.error.URLError, OSError):
        existing = _try_status(client, request["run_id"])
        if existing is not None:
            return existing
        raise


def _try_status(client: Client, run_id: str) -> Optional[dict[str, Any]]:
    try:
        return client.training_status(run_id)
    except MarketForgeError:
        return None


def drive_strategy(client: Client, room_id: str, account_id: int, remaining_qty: int) -> None:
    if remaining_qty <= 0:
        return
    observed = client.observe(room_id, account_id)
    observation = observed.get("observation", observed) if isinstance(observed, dict) else {}
    book = observation.get("book") if isinstance(observation, dict) else None
    asks = (book or {}).get("asks") or []
    if not asks:
        return
    top = asks[0]
    price = int(top["price_tick"])
    available = int(top.get("qty") or remaining_qty)
    qty = max(1, min(remaining_qty, available))
    cursor = client.cursor if client.cursor is not None else 0
    client.place(
        room_id,
        account_id,
        "buy",
        price,
        qty,
        idempotency_key=f"batch-buy-{room_id}-{account_id}-{cursor}-{qty}",
    )


def remaining_qty(payload: dict[str, Any]) -> int:
    run = payload.get("run") if isinstance(payload.get("run"), dict) else {}
    spec = run.get("spec") if isinstance(run.get("spec"), dict) else {}
    target = int(spec.get("target_qty") or 0)
    filled = int(run.get("filled_qty") or 0)
    return max(0, target - filled)


def drive_until_terminal(
    client: Client,
    request: dict[str, Any],
    payload: dict[str, Any],
    *,
    timeout_seconds: float,
    poll_interval: float,
    max_clock_advances: Optional[int] = None,
    cancel_run_ids: Optional[set[str]] = None,
    drive_strategy_enabled: bool = True,
) -> dict[str, Any]:
    run_id = request["run_id"]
    room_id = request["scenario"]["room_id"]
    account_id = int(request.get("trainee_account_id") or 0)
    deadline = time.monotonic() + timeout_seconds
    advances = 0
    plugin_agents = has_plugin_agents(request)
    if plugin_agents and not is_terminal_status(run_status(payload)):
        client.pause_room(room_id, idempotency_key=f"batch-pause-{run_id}")
    while time.monotonic() < deadline:
        if cancel_run_ids is not None and run_id in cancel_run_ids:
            try:
                return client.abort_training(run_id)
            except MarketForgeError as exc:
                looked = _try_status(client, run_id)
                if looked is not None and is_terminal_status(run_status(looked)):
                    return looked
                raise MarketForgeError(exc.status, f"cancel failed: {exc.body}") from exc
        status = run_status(payload)
        if is_terminal_status(status):
            return payload
        if drive_strategy_enabled and account_id and not (plugin_agents and has_trainee_agent(request)):
            try:
                drive_strategy(client, room_id, account_id, remaining_qty(payload))
            except MarketForgeError:
                pass
        if max_clock_advances is not None and advances >= max_clock_advances:
            return client.training_status(run_id)
        try:
            if plugin_agents:
                # Use the server's durable simulation step: decision + actions + state,
                # rather than advancing a clock while a wall-clock worker races it.
                # A cursor-derived key also survives a batch-process restart.
                cursor = client.clock(room_id)["clock"]["step"]
                client.step_bots(room_id, idempotency_key=f"batch-bots-{run_id}-{cursor}")
            else:
                client.advance_clock(
                    room_id,
                    1,
                    idempotency_key=f"batch-clock-{run_id}-{advances}",
                )
            advances += 1
        except MarketForgeError as exc:
            payload = client.training_status(run_id)
            if is_terminal_status(run_status(payload)):
                return payload
            if exc.status in {409, 429}:
                time.sleep(poll_interval)
                payload = client.training_status(run_id)
                continue
            raise
        payload = client.training_status(run_id)
        time.sleep(poll_interval)
    try:
        return client.abort_training(run_id)
    except MarketForgeError:
        looked = _try_status(client, run_id)
        if looked is not None:
            return looked
        raise MarketForgeError(408, f"training run {run_id} timed out")


def _score_from_payload(payload: dict[str, Any]) -> Optional[dict[str, Any]]:
    score = payload.get("score")
    return score if isinstance(score, dict) else None


def _versions_from_payload(payload: dict[str, Any]) -> dict[str, Any]:
    run = payload.get("run") if isinstance(payload.get("run"), dict) else {}
    spec = run.get("spec") if isinstance(run.get("spec"), dict) else {}
    return {
        "spec": spec.get("spec_version"),
        "task": spec.get("task_version"),
        "scoring": spec.get("scoring_version"),
    }


def _row_from_payload(
    *,
    seed: int,
    request: dict[str, Any],
    identity: dict[str, Any],
    payload: dict[str, Any],
    t0: float,
    ok: bool,
    failure_type: Optional[str],
    retries: int,
    error: Optional[str] = None,
) -> dict[str, Any]:
    run = payload.get("run") if isinstance(payload.get("run"), dict) else {}
    score = _score_from_payload(payload)
    status = str(run.get("status") or "")
    finished = is_terminal_status(status)
    q = None if score is None else score.get("q")
    metric_missing = score is None or q is None
    zero_fills = (not metric_missing) and q == 0
    phase = "completed" if ok and finished else ("failed" if finished or not ok else "running")
    row = {
        "seed": seed,
        "ok": bool(ok and finished and failure_type is None),
        "run_id": request["run_id"],
        "room_id": request["scenario"]["room_id"],
        "status": status or None,
        "phase": phase,
        "identity": identity,
        "score": score,
        "fills": None if not isinstance(run.get("fills"), list) else len(run["fills"]),
        "fees_paid": None if score is None else score.get("fees_paid"),
        "duration_ms": int((time.monotonic() - t0) * 1000),
        "failure_type": failure_type,
        "retries": retries,
        "versions": _versions_from_payload(payload),
        "zero_fills": zero_fills,
        "metric_missing": metric_missing,
        "low_cost_win": False if zero_fills or metric_missing else None,
        "server_run_id": run.get("spec", {}).get("run_id") if isinstance(run.get("spec"), dict) else request["run_id"],
    }
    if error:
        row["error"] = error
    if not finished:
        row["ok"] = False
        row["phase"] = "running"
    return row


class StateStore:
    """Single-writer store: flock + temp file + os.replace."""

    def __init__(self, path: Optional[Path]):
        self.path = Path(path) if path else None
        self._thread_lock = threading.Lock()
        self._lock_file = None

    def acquire(self) -> None:
        if self.path is None:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        lock_path = self.path.with_name(self.path.name + ".lock")
        self._lock_file = open(lock_path, "a+", encoding="utf-8")
        try:
            import fcntl

            fcntl.flock(self._lock_file.fileno(), fcntl.LOCK_EX)
        except ImportError:
            pass

    def release(self) -> None:
        if self._lock_file is None:
            return
        try:
            import fcntl

            fcntl.flock(self._lock_file.fileno(), fcntl.LOCK_UN)
        except ImportError:
            pass
        self._lock_file.close()
        self._lock_file = None

    def load(self) -> dict[str, Any]:
        if self.path is None or not self.path.exists():
            return {"runs": []}
        try:
            data = json.loads(self.path.read_text())
        except (OSError, json.JSONDecodeError, UnicodeDecodeError):
            return {"runs": [], "corrupt": True}
        if not isinstance(data, dict):
            return {"runs": [], "corrupt": True}
        runs = data.get("runs")
        if not isinstance(runs, list):
            return {"runs": [], "corrupt": True}
        return data

    def save(self, state: dict[str, Any]) -> None:
        if self.path is None:
            return
        with self._thread_lock:
            self._atomic_write(state)

    def upsert(self, row: dict[str, Any]) -> dict[str, Any]:
        with self._thread_lock:
            state = self.load()
            runs = list(state.get("runs") or [])
            run_id = row.get("run_id")
            replaced = False
            for index, existing in enumerate(runs):
                if isinstance(existing, dict) and existing.get("run_id") == run_id:
                    runs[index] = row
                    replaced = True
                    break
            if not replaced:
                runs.append(row)
            state["runs"] = runs
            if self.path is not None:
                self._atomic_write(state)
            return state

    def _atomic_write(self, state: dict[str, Any]) -> None:
        assert self.path is not None
        self.path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp_name = tempfile.mkstemp(
            dir=str(self.path.parent),
            prefix=f".{self.path.name}.",
            suffix=".tmp",
        )
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(state, handle, indent=2, default=str)
                handle.write("\n")
                handle.flush()
                os.fsync(handle.fileno())
            os.replace(tmp_name, self.path)
        except Exception:
            try:
                os.unlink(tmp_name)
            except OSError:
                pass
            raise


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    failures = [row for row in rows if not row.get("ok")]
    completed = [row for row in rows if row.get("status") == "Completed"]
    failure_types = [row.get("failure_type") or row.get("error") or row.get("status") for row in failures]
    costs: list[Any] = []
    fees: list[Any] = []
    durations = [row.get("duration_ms") for row in rows]
    versions = [row.get("versions") for row in rows]
    for row in rows:
        score = row.get("score") if isinstance(row.get("score"), dict) else None
        if score is None:
            row["metric_missing"] = True
            costs.append(None)
            fees.append(None)
            continue
        q = score.get("q")
        if q is None:
            row["metric_missing"] = True
        elif q == 0:
            row["zero_fills"] = True
            row["low_cost_win"] = False
        costs.append(score.get("buy_slippage_bp"))
        fees.append(score.get("fees_paid"))
    n = len(rows)
    return {
        "runs": rows,
        "failures": failures,
        "n": n,
        "completed": len(completed),
        "completion_rate": (len(completed) / n) if n else None,
        "costs": costs,
        "fees": fees,
        "durations_ms": durations,
        "failure_types": failure_types,
        "versions": versions,
        "zero_fills_not_win": True,
        "sample_size": n,
        "failed_kept": len(failures),
    }


def evaluate_one(
    client: Client,
    spec: dict[str, Any],
    seed: int,
    *,
    request: Optional[dict[str, Any]] = None,
    identity: Optional[dict[str, Any]] = None,
    existing: Optional[dict[str, Any]] = None,
    timeout_seconds: float = 60,
    poll_interval: float = 0.05,
    max_clock_advances: Optional[int] = None,
    cancel_run_ids: Optional[set[str]] = None,
    fail_seeds: Optional[set[int]] = None,
    drive_strategy_enabled: bool = True,
    store: Optional[StateStore] = None,
    on_start: Hook = None,
    on_running: Hook = None,
    on_finish: Hook = None,
    active_counter: Optional[list[int]] = None,
    active_lock: Optional[threading.Lock] = None,
) -> dict[str, Any]:
    request = request or prepare_request(spec, seed)
    identity = identity or experiment_identity(spec, seed)
    retries = int((existing or {}).get("retries") or 0)
    t0 = time.monotonic()
    if active_lock is not None and active_counter is not None:
        with active_lock:
            active_counter[0] += 1
            active_counter[1] = max(active_counter[1], active_counter[0])
    try:
        if fail_seeds and seed in fail_seeds:
            try:
                payload = start_or_lookup(client, request)
                if on_start:
                    on_start({"seed": seed, "run_id": request["run_id"], "payload": payload})
                if not is_terminal_status(run_status(payload)):
                    payload = client.abort_training(request["run_id"])
                row = _row_from_payload(
                    seed=seed,
                    request=request,
                    identity=identity,
                    payload=payload,
                    t0=t0,
                    ok=False,
                    failure_type="injected_fail_seed",
                    retries=retries,
                )
            except MarketForgeError as exc:
                row = {
                    "seed": seed,
                    "ok": False,
                    "run_id": request["run_id"],
                    "room_id": request["scenario"]["room_id"],
                    "status": None,
                    "phase": "failed",
                    "identity": identity,
                    "score": None,
                    "error": str(exc),
                    "failure_type": "injected_fail_seed",
                    "retries": retries,
                    "duration_ms": int((time.monotonic() - t0) * 1000),
                    "metric_missing": True,
                    "zero_fills": False,
                    "low_cost_win": False,
                    "versions": {},
                }
            if store is not None:
                store.upsert(row)
            if on_finish:
                on_finish(row)
            return row

        payload = start_or_lookup(client, request)
        if on_start:
            on_start({"seed": seed, "run_id": request["run_id"], "payload": payload})
        running_row = _row_from_payload(
            seed=seed,
            request=request,
            identity=identity,
            payload=payload,
            t0=t0,
            ok=False,
            failure_type=None,
            retries=retries,
        )
        if store is not None:
            store.upsert(running_row)
        if on_running:
            on_running(running_row)
        if not is_terminal_status(run_status(payload)):
            payload = drive_until_terminal(
                client,
                request,
                payload,
                timeout_seconds=timeout_seconds,
                poll_interval=poll_interval,
                max_clock_advances=max_clock_advances,
                cancel_run_ids=cancel_run_ids,
                drive_strategy_enabled=drive_strategy_enabled,
            )
        status = run_status(payload)
        timeout_abort = status == "Aborted" and max_clock_advances is None
        failure_type = None
        ok = is_terminal_status(status) and status == "Completed"
        if status == "Aborted":
            failure_type = "aborted"
            ok = False
        elif status == "Failed":
            failure_type = "failed"
            ok = False
        elif not is_terminal_status(status):
            failure_type = "in_progress"
            ok = False
        if timeout_abort and remaining_qty({"run": payload.get("run"), "score": payload.get("score")}) > 0:
            # abort-on-timeout is a failure even if the server marks Aborted cleanly
            failure_type = failure_type or "timeout"
        row = _row_from_payload(
            seed=seed,
            request=request,
            identity=identity,
            payload=payload,
            t0=t0,
            ok=ok,
            failure_type=failure_type,
            retries=retries,
        )
        if store is not None:
            store.upsert(row)
        if on_finish:
            on_finish(row)
        return row
    except MarketForgeError as exc:
        row = {
            "seed": seed,
            "ok": False,
            "run_id": request["run_id"],
            "room_id": request["scenario"]["room_id"],
            "status": None,
            "phase": "failed",
            "identity": identity,
            "score": None,
            "error": str(exc),
            "failure_type": "http",
            "retries": retries + 1,
            "duration_ms": int((time.monotonic() - t0) * 1000),
            "metric_missing": True,
            "zero_fills": False,
            "low_cost_win": False,
            "versions": {},
        }
        if store is not None:
            store.upsert(row)
        if on_finish:
            on_finish(row)
        return row
    finally:
        if active_lock is not None and active_counter is not None:
            with active_lock:
                active_counter[0] -= 1


def run_batch(
    client: Client,
    spec: dict[str, Any],
    seeds: Iterable[int],
    *,
    state_path: Optional[Path] = None,
    concurrency: int = 1,
    timeout_seconds: float = 60,
    poll_interval: float = 0.05,
    max_steps: Optional[int] = None,
    max_clock_advances: Optional[int] = None,
    max_retries: int = 1,
    fail_seeds: Optional[Iterable[int]] = None,
    cancel_run_ids: Optional[set[str]] = None,
    run_prefix: Optional[str] = None,
    drive_strategy_enabled: bool = True,
    on_start: Hook = None,
    on_running: Hook = None,
    on_finish: Hook = None,
    active_counter: Optional[list[int]] = None,
    active_lock: Optional[threading.Lock] = None,
) -> dict[str, Any]:
    seeds = [int(seed) for seed in seeds]
    fail_set = {int(item) for item in (fail_seeds or [])}
    cancel_ids = cancel_run_ids if cancel_run_ids is not None else set()
    store = StateStore(state_path)
    store.acquire()
    try:
        state = store.load()
        by_run: dict[str, dict[str, Any]] = {}
        for row in state.get("runs") or []:
            if isinstance(row, dict) and row.get("run_id"):
                by_run[str(row["run_id"])] = row
        planned: list[tuple[str, Optional[dict[str, Any]], dict[str, Any], dict[str, Any]]] = []
        for seed in seeds:
            request = prepare_request(spec, seed, run_prefix=run_prefix, max_steps=max_steps)
            identity = experiment_identity(spec, seed)
            existing = by_run.get(request["run_id"])
            if existing is not None:
                same = (existing.get("identity") or {}).get("digest") == identity["digest"]
                retries = int(existing.get("retries") or 0)
                if same and is_terminal_row(existing) and (
                    existing.get("ok") or retries >= max_retries or existing.get("failure_type") == "injected_fail_seed"
                ):
                    planned.append(("reuse", existing, request, identity))
                    continue
            planned.append(("eval", existing, request, identity))

        results: dict[int, dict[str, Any]] = {}
        pending: list[tuple[Optional[dict[str, Any]], dict[str, Any], dict[str, Any]]] = []
        for kind, existing, request, identity in planned:
            if kind == "reuse" and existing is not None:
                results[int(existing["seed"])] = {**existing, "resumed": True}
            else:
                pending.append((existing, request, identity))

        workers = max(1, int(concurrency))

        def work(item: tuple[Optional[dict[str, Any]], dict[str, Any], dict[str, Any]]) -> dict[str, Any]:
            existing, request, identity = item
            return evaluate_one(
                client,
                spec,
                int(identity["seed"]),
                request=request,
                identity=identity,
                existing=existing,
                timeout_seconds=timeout_seconds,
                poll_interval=poll_interval,
                max_clock_advances=max_clock_advances,
                cancel_run_ids=cancel_ids,
                fail_seeds=fail_set,
                drive_strategy_enabled=drive_strategy_enabled,
                store=store,
                on_start=on_start,
                on_running=on_running,
                on_finish=on_finish,
                active_counter=active_counter,
                active_lock=active_lock,
            )

        if workers == 1 or len(pending) <= 1:
            for item in pending:
                row = work(item)
                results[int(row["seed"])] = row
        else:
            with ThreadPoolExecutor(max_workers=workers) as pool:
                futs = [pool.submit(work, item) for item in pending]
                for fut in as_completed(futs):
                    row = fut.result()
                    results[int(row["seed"])] = row

        rows = [results[seed] for seed in seeds if seed in results]
        summary = summarize(rows)
        snapshot = store.load()
        snapshot["runs"] = rows
        snapshot["summary"] = {
            "n": summary["n"],
            "completed": summary["completed"],
            "completion_rate": summary["completion_rate"],
            "failed_kept": summary["failed_kept"],
            "zero_fills_not_win": True,
        }
        store.save(snapshot)
        return summary
    finally:
        store.release()


def build_client(base_url: str, bearer: Optional[str], timeout: float) -> Client:
    return Client(
        base_url,
        bearer=bearer,
        trusted_owner_urls=[base_url.rstrip("/")],
        timeout=timeout,
    )


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Run isolated training seeds to terminal status")
    parser.add_argument("base_url")
    parser.add_argument("spec")
    parser.add_argument("seeds", nargs="*", type=int)
    parser.add_argument("--state", help="JSON file used to resume runs (atomic replace, single writer)")
    parser.add_argument("--max-steps", type=int, default=None, help="override horizon_steps")
    parser.add_argument("--fail-seeds", default="", help="comma-separated seeds aborted after start (spec unchanged)")
    parser.add_argument("--concurrency", type=int, default=1)
    parser.add_argument("--timeout-seconds", type=float, default=60)
    parser.add_argument("--poll-interval", type=float, default=0.05)
    parser.add_argument("--max-retries", type=int, default=1)
    parser.add_argument("--bearer", default=os.environ.get("MARKETFORGE_BEARER"))
    parser.add_argument("--run-prefix", default=None)
    parser.add_argument("--cancel-run-id", action="append", default=[])
    parser.add_argument("--no-drive-strategy", action="store_true")
    args = parser.parse_args(argv)

    spec = json.loads(Path(args.spec).read_text())
    seeds = args.seeds or [1]
    fail_seeds = {int(item) for item in args.fail_seeds.split(",") if item.strip()}
    client = build_client(args.base_url, args.bearer, timeout=max(args.timeout_seconds, 30))
    summary = run_batch(
        client,
        spec,
        seeds,
        state_path=Path(args.state) if args.state else None,
        concurrency=args.concurrency,
        timeout_seconds=args.timeout_seconds,
        poll_interval=args.poll_interval,
        max_steps=args.max_steps,
        max_retries=args.max_retries,
        fail_seeds=fail_seeds,
        cancel_run_ids=set(args.cancel_run_id),
        run_prefix=args.run_prefix,
        drive_strategy_enabled=not args.no_drive_strategy,
    )
    print(json.dumps(summary, indent=2, default=str))
    return 0 if not summary["failures"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
