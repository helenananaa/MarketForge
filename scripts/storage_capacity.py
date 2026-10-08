#!/usr/bin/env python3
"""Measure actual HTTP journal workload against an EMPTY isolated PostgreSQL DB.

Starts only its own server, generates place/fill/cancel/clock records, closes
rooms, measures physical and logical storage, restarts, and compares recovered
state. A short run is a capacity sample, not a production throughput guarantee.
"""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import socket
import statistics
import subprocess
import sys
import time
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import uuid

from storage_archive import catalog, encoded, identifier, literal, pg_json

ROOT = Path(__file__).resolve().parents[1]


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, int(len(ordered) * fraction + 0.999999) - 1))]


def scenario(room_id):
    return {"room_id": room_id, "market": {"Spot": {
        "instrument": {"instrument_id": "V-BTC-SPOT", "venue_id": "default-venue", "symbol": "V-BTC-SPOT",
                       "base_asset": "V", "quote_asset": "BTC", "tick_size": 1, "lot_size": 1},
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {"allow_short": True}}},
        "accounts": [{"Spot": {"account_id": 10, "cash_balance": 10**12, "position_qty": 10**9}},
                     {"Spot": {"account_id": 20, "cash_balance": 10**12, "position_qty": 0}}], "seed_orders": []}


def request(base, path, payload=None, key=None):
    headers = {"content-type": "application/json", "x-user-id": "storage-benchmark"}
    if key:
        headers["idempotency-key"] = key
    req = Request(base + path, data=encoded(payload) if payload is not None else None, headers=headers,
                  method="POST" if payload is not None else "GET")
    started = time.perf_counter()
    try:
        with urlopen(req, timeout=120) as response:
            result = json.load(response)
    except HTTPError as error:
        raise RuntimeError(f"HTTP {error.code} at {path}: {error.read().decode()}") from error
    return result, (time.perf_counter() - started) * 1000


def stop_server(proc):
    if proc is not None and proc.poll() is None:
        proc.terminate()
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def start_server(env, port, log):
    env = {**env, "MARKETFORGE_BIND_ADDR": f"127.0.0.1:{port}", "MARKETFORGE_RUNTIME_MODE": "single-active",
           "MARKETFORGE_RUNTIME_LOCK_WAIT_MS": "0", "MARKETFORGE_JOURNAL_READ_WORKERS": "4",
           "MARKETFORGE_INSTANCE_ID": "storage-benchmark"}
    started = time.perf_counter()
    proc = subprocess.Popen([str(ROOT / "target/debug/exchange-server")], env=env, stdout=log, stderr=log)
    base = f"http://127.0.0.1:{port}"
    for _ in range(1200):
        if proc.poll() is not None:
            raise RuntimeError("benchmark server exited; inspect the saved server log")
        try:
            with urlopen(base + "/health/ready", timeout=0.2) as response:
                if response.status == 200:
                    return proc, (time.perf_counter() - started) * 1000
        except (URLError, TimeoutError):
            time.sleep(0.05)
    stop_server(proc)
    raise RuntimeError("benchmark server did not become ready")


def audit(env, output, mode="database"):
    with open(output, "wb") as stream:
        subprocess.run([str(ROOT / "target/debug/journal-audit"), mode], env=env, stdout=stream, check=True, timeout=300)
    report = json.loads(Path(output).read_bytes())
    state = report.pop("state")
    report["state_sha256"] = hashlib.sha256(encoded(state)).hexdigest()
    report["state_bytes"] = len(encoded(state))
    return report


def storage_stats(env_name):
    result = []
    for table in catalog(env_name):
        name = table["table"]
        rows = pg_json(f"""SELECT jsonb_build_object('table',{literal(name)}, 'rows',count(*),
          'row_storage_bytes',COALESCE(sum(pg_column_size(t)),0),
          'physical_bytes',pg_total_relation_size({literal('public.' + name)}),
          'index_bytes',pg_indexes_size({literal('public.' + name)})) FROM {identifier(name)} t;""", env_name)[0]
        result.append(rows)
    snapshots = pg_json("""SELECT jsonb_build_object('room_id',room_id,'command_seq',command_seq,
        'actor_json_bytes',octet_length(actor_json::text)) FROM marketforge_room_snapshots ORDER BY room_id,command_seq;""", env_name)
    return result, snapshots


def workload(base, room, cycles):
    latencies = []
    def call(path, payload, key):
        result, elapsed = request(base, path, payload, key)
        latencies.append(elapsed)
        return result
    def order(account, action, key):
        result = call(f"/rooms/{room}/orders", {"participant_id": f"bench-{account}", "account_id": account, "action": action}, key)
        if not result["accepted"]:
            raise RuntimeError(f"benchmark order rejected: {result}")
        return result
    for cycle in range(cycles):
        key = f"{room}-{cycle}"
        order(10, {"PlaceLimit": {"side": "Sell", "price_tick": 101, "qty": 1}}, key + "-maker")
        order(20, {"PlaceLimit": {"side": "Buy", "price_tick": 101, "qty": 1}}, key + "-fill")
        resting = order(20, {"PlaceLimit": {"side": "Buy", "price_tick": 99, "qty": 1}}, key + "-rest")
        order_id = next(event["order_id"] for event in resting["events"] if event["type"] == "OrderAccepted")
        order(20, {"Cancel": {"order_id": order_id}}, key + "-cancel")
        call(f"/rooms/{room}/clock/advance", {"steps": 1}, key + "-clock")
    return latencies


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--rooms", type=int, required=True)
    parser.add_argument("--cycles", type=int, default=100)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--isolated-target", action="store_true", required=True)
    args = parser.parse_args()
    if not 1 <= args.rooms <= 128 or not 1 <= args.cycles <= 100000:
        parser.error("rooms must be 1..128 and cycles 1..100000")
    args.output.mkdir(parents=True, exist_ok=False)
    env_name = "MARKETFORGE_DATABASE_URL"
    env = os.environ.copy()
    # Reject an existing MarketForge workload; never benchmark an operational DB.
    rows = pg_json("SELECT jsonb_build_object('present',to_regclass('public.marketforge_rooms') IS NOT NULL);", env_name)[0]
    if rows["present"] and pg_json("SELECT to_jsonb(count(*)) FROM marketforge_rooms;", env_name)[0]:
        raise ValueError("capacity benchmark requires an empty isolated database")
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    base = f"http://127.0.0.1:{port}"
    run_id = uuid.uuid4().hex[:10]
    room_ids = [f"storage-{run_id}-{i:03}" for i in range(args.rooms)]
    proc = None
    with open(args.output / "server.log", "wb") as log:
        try:
            proc, initial_start_ms = start_server(env, port, log)
            for room in room_ids:
                request(base, "/rooms", {"scenario": scenario(room), "agents": [], "autostart_agents": False})
            wal_before = pg_json("SELECT to_jsonb(pg_current_wal_insert_lsn()::text);", env_name)[0]
            before_tables, _ = storage_stats(env_name)
            started = time.perf_counter()
            with ThreadPoolExecutor(max_workers=args.rooms) as executor:
                futures = [executor.submit(workload, base, room, args.cycles) for room in room_ids]
                latencies = [latency for future in futures for latency in future.result()]
            duration = time.perf_counter() - started
            wal_bytes = int(pg_json(f"SELECT to_jsonb(pg_wal_lsn_diff(pg_current_wal_insert_lsn(),{literal(wal_before)}::pg_lsn));", env_name)[0])
            views = {room: {path: request(base, f"/rooms/{room}/{path}")[0] for path in ("ticker", "candles")} for room in room_ids}
            for room in room_ids:
                request(base, f"/rooms/{room}/close", {})
            tables, snapshots = storage_stats(env_name)
            before = audit(env, args.output / "before-restart-state.json")
            metrics = urlopen(base + "/metrics", timeout=10).read()
            (args.output / "metrics.txt").write_bytes(metrics)
            stop_server(proc)
            proc = None
            proc, restart_ready_ms = start_server(env, port, log)
            after = audit(env, args.output / "after-restart-state.json")
            runtime = audit(env, args.output / "runtime-recovery-state.json", "runtime")
            if before["state_sha256"] != after["state_sha256"]:
                raise RuntimeError("state differs after restart")
            if runtime["state_sha256"] != after["state_sha256"]:
                raise RuntimeError("checkpoint recovery differs from full replay")
            for room in room_ids:
                for path in ("ticker", "candles"):
                    if request(base, f"/rooms/{room}/{path}")[0] != views[room][path]:
                        raise RuntimeError(f"{path} differs after restart")
            commands = args.rooms * args.cycles * 4
            total_before = sum(t["physical_bytes"] for t in before_tables)
            total_after = sum(t["physical_bytes"] for t in tables)
            report = {"format": "marketforge.storage-capacity.v1", "rooms": args.rooms, "cycles_per_room": args.cycles,
                      "room_ids": room_ids, "duration_seconds": duration, "order_commands": commands,
                      "http_mutations": len(latencies), "order_commands_per_second": commands / duration,
                      "http_mutations_per_second": len(latencies) / duration,
                      "latency_ms": {"mean": statistics.mean(latencies), "p50": percentile(latencies, .5),
                                     "p95": percentile(latencies, .95), "p99": percentile(latencies, .99), "max": max(latencies)},
                      "physical_growth_bytes": total_after - total_before,
                      "physical_growth_bytes_per_order_command": (total_after - total_before) / commands,
                      "wal_bytes": wal_bytes, "wal_bytes_per_order_command": wal_bytes / commands,
                      "initial_ready_ms": initial_start_ms, "restart_ready_ms": restart_ready_ms,
                      "recovery": after, "runtime_recovery": runtime, "state_and_market_views_equal_after_restart": True,
                      "tables": tables, "snapshots": snapshots,
                      "limitations": ["debug build", "closed-loop clients", "short sample", "one process; shared application lock and journal writer", "WAL is cluster-wide; use a dedicated cluster", "excludes backups and replicas"]}
            (args.output / "report.json").write_bytes(encoded(report))
            print(json.dumps({key: value for key, value in report.items() if key not in {"tables", "snapshots", "room_ids"}}, ensure_ascii=False))
        finally:
            stop_server(proc)


if __name__ == "__main__":
    main()
