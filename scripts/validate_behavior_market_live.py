"""Qualify the full native population and, optionally, an isolated PostgreSQL restart.

The PostgreSQL data directory must belong to this validation and be stopped.
Existing exchange/database processes are never attached to or stopped.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from marketforge import Client
from behavior_market import behavior_recipe
from microstructure_market import microstructure_recipe


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")


def unwrapped(value):
    return value.get("observation", value)


def states(saved):
    return {a["template"]["Plugin"]["participant"]["participant_id"]:
            a["kind_state"]["Plugin"]["data"] for a in saved["agents"]}


def peak_memory(process):
    if os.name != "nt":
        return None
    import ctypes
    from ctypes import wintypes
    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
            (name, ctypes.c_size_t) for name in ("PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage",
                "QuotaPagedPoolUsage", "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage", "PeakPagefileUsage")]
    counter = Counters()
    counter.cb = ctypes.sizeof(counter)
    get_memory = ctypes.windll.psapi.GetProcessMemoryInfo
    get_memory.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    get_memory.restype = wintypes.BOOL
    if not get_memory(wintypes.HANDLE(int(process._handle)), ctypes.byref(counter), counter.cb):
        raise ctypes.WinError()
    return counter.PeakWorkingSetSize


def snapshot(client, room, spec):
    return {"scheduler": client.room_bots(room), "clock": client.clock(room),
            "accounts": {a["Plugin"]["participant"]["participant_id"]: unwrapped(client.observe(
                room, a["Plugin"]["participant"]["account_id"],
                a["Plugin"]["participant"]["instrument_id"])) for a in spec["agents"]}}


def run(args):
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    binary = args.server.resolve()
    pg_data = args.postgres_data.resolve() if args.postgres_data else None
    if pg_data and (pg_data / "postmaster.pid").exists():
        raise RuntimeError("validation PostgreSQL data directory is already running; refusing to attach")
    pg_port = port() if pg_data else None
    pg_database = "behavior_" + uuid.uuid4().hex if pg_data else None
    pg_running = False
    process = None
    log = None
    phase = "startup"
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MARKETFORGE_", "PG"))}
    env["PATH"] = str(Path(sys.executable).parent) + os.pathsep + env["PATH"]
    if pg_data:
        env["MARKETFORGE_DATABASE_URL"] = f"postgresql://forge@127.0.0.1:{pg_port}/{pg_database}"
    room = "behavior-qualification-" + uuid.uuid4().hex[:10]
    spec = (microstructure_recipe if args.microstructure else behavior_recipe)(room, args.seed)
    spec["agent_interval_ms"] = args.interval_ms
    if args.compressed_timeline:
        for event, publish, expiry in zip(spec["scenario"]["market_events"], [5000, 20000], [15000, 40000]):
            event.update(published_at_ms=publish, expires_at_ms=expiry)
        for agent in spec["agents"]:
            if agent["Plugin"]["plugin_id"] == "PovExecutionTrader":
                agent["Plugin"]["config"].update(horizon_ms=30000, deadline_urgency_ms=5000)
    save(output / "spec.json", spec)

    def pg(action):
        nonlocal pg_running
        command = [str(args.postgres_bin / "pg_ctl.exe" if os.name == "nt" else args.postgres_bin / "pg_ctl"),
                   "-D", str(pg_data), "-w", "-t", "20"]
        if action == "start":
            command += ["-l", str(output / "postgres.log"), "-o", f"-h 127.0.0.1 -p {pg_port}", "start"]
        else:
            command += ["-m", "fast", "stop"]
        with (output / "postgres-control.log").open("a", encoding="utf-8") as receipt:
            # A Windows PostgreSQL child can inherit pipe handles from pg_ctl;
            # a real log file avoids waiting for that daemon to close a pipe.
            result = subprocess.run(command, env=env, stdout=receipt, stderr=subprocess.STDOUT,
                                    timeout=25, creationflags=flags)
        result.check_returncode()
        pg_running = action == "start"

    def start_server(label):
        nonlocal process, log
        server_port = port()
        local_env = dict(env, MARKETFORGE_BIND_ADDR=f"127.0.0.1:{server_port}",
                         MARKETFORGE_AUTH_TOKENS_JSON='{"behavior-validation":"behavior-owner"}')
        log = (output / f"{label}.log").open("w", encoding="utf-8")
        process = subprocess.Popen([str(binary)], cwd=output, env=local_env, stdout=log,
                                   stderr=subprocess.STDOUT, creationflags=flags)
        client = Client(f"http://127.0.0.1:{server_port}", bearer="behavior-validation", timeout=15)
        deadline = time.monotonic() + 25
        while True:
            try:
                client.health_ready()
                return client
            except (RuntimeError, OSError):
                if process.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError(f"{label} readiness failed")
                time.sleep(0.05)

    def stop_server():
        nonlocal process, log
        if process and process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        process = None
        if log:
            log.close()
            log = None

    def advance_to(client, target):
        started = time.monotonic()
        while True:
            status = client._request("GET", f"/rooms/{room}/agents")
            observation = unwrapped(client.observe(room, 420, "V-BTC-SPOT"))
            if status["bot_errors"] or status.get("lifecycle") == "failed":
                save(output / "worker-failure.json", {"status": status, "observation": observation})
                raise AssertionError("native worker failed")
            if observation["step"] >= target:
                return {"step": observation["step"], "wall_seconds": time.monotonic() - started, "status": status}
            if process.poll() is not None or time.monotonic() - started > args.timeout_seconds:
                save(output / "timeout.json", {"status": status, "observation": observation,
                    "peak_memory_bytes": peak_memory(process) if process.poll() is None else None})
                raise AssertionError(f"clock did not reach {target} within {args.timeout_seconds}s")
            time.sleep(0.05)

    try:
        if pg_data:
            pg("start")
            # A fresh database prevents an earlier failed validation room's
            # automatically recovered worker from competing with this run.
            createdb = args.postgres_bin / ("createdb.exe" if os.name == "nt" else "createdb")
            with (output / "postgres-control.log").open("a", encoding="utf-8") as receipt:
                subprocess.run([str(createdb), "-h", "127.0.0.1", "-p", str(pg_port), "-U", "forge", pg_database],
                               env=env, stdout=receipt, stderr=subprocess.STDOUT, timeout=20,
                               creationflags=flags, check=True)
        client = start_server("initial-server")
        client._request("POST", "/rooms", spec)
        phase = "full-population"
        progress = advance_to(client, args.steps)
        client.pause_room(room)
        before = snapshot(client, room, spec)
        save(output / "before.json", before)
        initial_peak = peak_memory(process)
        save(output / "resources.json", {"peak_memory_bytes": initial_peak, "progress": progress})
        if args.max_memory_mib is not None:
            assert initial_peak is not None, "memory threshold requires Windows process counters"
            assert initial_peak <= args.max_memory_mib * 1024**2, f"memory peak {initial_peak / 1024**2:.1f}MiB exceeded {args.max_memory_mib}MiB"
        saved = states(before["scheduler"])
        assert len(saved) == len(spec["agents"])
        for name in ("pov-buy", "pov-sell"):
            assert 0 <= saved[name]["completed_qty"] <= 25, (name, saved[name])
            if args.steps >= 60:
                assert saved[name]["deadline_reached"], name
                assert saved[name]["completed_qty"] > 0, name
        for i in range(4):
            assert saved[f"event-{i}"]["received_event_ids"] == ["news-up", "news-down"]
        lags = [before["accounts"][name]["market_time_ms"] - state["last_observation"][1]
                for name, state in saved.items() if state.get("last_observation")]
        recovered = None
        resumed = None
        if pg_data:
            phase = "postgres-and-server-restart"
            stop_server()
            pg("stop")
            pg("start")
            client = start_server("recovered-server")
            after = snapshot(client, room, spec)
            save(output / "after.json", after)
            assert after == before, "durable scheduler, clock, books, account observations or messages changed on restart"
            recovered = {"exact_snapshot_match": True, "accounts_and_bot_legs": len(after["accounts"])}
            phase = "resume-after-recovery"
            client._request("POST", f"/rooms/{room}/resume", {})
            resumed = advance_to(client, max(progress["step"] + 20, 60 if args.compressed_timeline else 260))
            client.pause_room(room)
            resumed_state = snapshot(client, room, spec)
            save(output / "resumed.json", resumed_state)
            resumed_bots = states(resumed_state["scheduler"])
            for name in ("pov-buy", "pov-sell"):
                if saved[name]["deadline_reached"]:
                    assert resumed_bots[name]["completed_qty"] == saved[name]["completed_qty"], "expired POV traded after recovery"
                else:
                    assert saved[name]["completed_qty"] <= resumed_bots[name]["completed_qty"] <= 25
                assert resumed_bots[name]["completed_qty"] > 0 and resumed_bots[name]["deadline_reached"]
            for i in range(4):
                assert resumed_bots[f"event-{i}"]["received_event_ids"] == ["news-up", "news-down"]
        report = {"passed": True, "seed": args.seed, "bots": len(spec["agents"]), "interval_ms": args.interval_ms,
                  "full_original_timeline": not args.compressed_timeline, "order_ttl_extended": False, "progress": progress,
                  "pov_filled": {name: saved[name]["completed_qty"] for name in ("pov-buy", "pov-sell")},
                  "observation_lag_sim_ms": {"max": max(lags), "min": min(lags)},
                  "postgres_restart": recovered, "resumed": resumed,
                  "pov_resumed_filled": {name: resumed_bots[name]["completed_qty"] for name in ("pov-buy", "pov-sell")} if resumed else None,
                  "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                  "initial_server_peak_memory_bytes": initial_peak,
                  "qualification": "local synthetic simulation; no real-market calibration"}
        save(output / "report.json", report)
        return report
    except Exception as error:
        save(output / "failure.json", {"passed": False, "phase": phase, "error": str(error)})
        raise
    finally:
        stop_server()
        if pg_running:
            pg("stop")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, default=ROOT / "target/debug/exchange-server.exe")
    parser.add_argument("--output", type=Path, default=ROOT / ".local/behavior-fast-qualification")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--steps", type=int, default=260)
    parser.add_argument("--interval-ms", type=int, default=25)
    parser.add_argument("--timeout-seconds", type=float, default=45)
    parser.add_argument("--max-memory-mib", type=float)
    parser.add_argument("--postgres-bin", type=Path, default=Path("D:/SQL/bin"))
    parser.add_argument("--postgres-data", type=Path)
    parser.add_argument("--compressed-timeline", action="store_true", help="shortened message/task schedule for recovery during an unfinished POV")
    parser.add_argument("--microstructure", action="store_true", help="include book-feedback makers and five perpetual motive traders")
    args = parser.parse_args()
    if args.steps < (30 if args.compressed_timeline else 260):
        parser.error("full-timeline qualification requires at least 260 steps")
    print(json.dumps(run(args), ensure_ascii=False))


if __name__ == "__main__":
    main()
