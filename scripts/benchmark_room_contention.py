"""Fixed-work HTTP load for room execution boundaries, with receipt validation.

Fresh loopback process per case; no existing server/database is used. All workers
share the same barrier, each has a persistent HTTP connection and exclusive
account(s). Closed-loop latency includes server queuing, not open-loop SLOs.
"""
import argparse
import concurrent.futures
import ctypes
from ctypes import wintypes
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from benchmark_market_load import cpu_seconds, percentiles
from validate_behavior_market_live import peak_memory, port, save


def process_snapshot():
    """Sample process identity and CPU, without reading commands or credentials."""
    if os.name != "nt":
        return []
    class Entry(ctypes.Structure):
        _fields_ = [("dwSize", wintypes.DWORD), ("cntUsage", wintypes.DWORD),
                    ("th32ProcessID", wintypes.DWORD), ("th32DefaultHeapID", ctypes.c_size_t),
                    ("th32ModuleID", wintypes.DWORD), ("cntThreads", wintypes.DWORD),
                    ("th32ParentProcessID", wintypes.DWORD), ("pcPriClassBase", wintypes.LONG),
                    ("dwFlags", wintypes.DWORD), ("szExeFile", wintypes.WCHAR * 260)]
    kernel = ctypes.windll.kernel32
    kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE
    kernel.Process32FirstW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.Process32NextW.argtypes = [wintypes.HANDLE, ctypes.POINTER(Entry)]
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    kernel.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    handle = kernel.CreateToolhelp32Snapshot(2, 0)
    if handle == wintypes.HANDLE(-1).value:
        return []
    result = []
    try:
        entry = Entry()
        entry.dwSize = ctypes.sizeof(entry)
        ok = kernel.Process32FirstW(handle, ctypes.byref(entry))
        while ok:
            process = kernel.OpenProcess(0x1000, False, entry.th32ProcessID)
            if process:
                try:
                    times = [wintypes.FILETIME() for _ in range(4)]
                    if kernel.GetProcessTimes(process, *[ctypes.byref(t) for t in times]):
                        ticks = [(t.dwHighDateTime << 32) + t.dwLowDateTime for t in times]
                        result.append({"pid": entry.th32ProcessID, "name": entry.szExeFile,
                                       "created_ticks": ticks[0], "cpu_seconds": sum(ticks[2:]) / 1e7})
                finally:
                    kernel.CloseHandle(process)
            ok = kernel.Process32NextW(handle, ctypes.byref(entry))
    finally:
        kernel.CloseHandle(handle)
    return result


def background_delta(before, after, excluded):
    initial = {(p["pid"], p["created_ticks"]): p for p in before}
    result = []
    for p in after:
        old = initial.get((p["pid"], p["created_ticks"]))
        if old and p["pid"] not in excluded:
            seconds = p["cpu_seconds"] - old["cpu_seconds"]
            if seconds > .05:
                result.append({"pid": p["pid"], "name": p["name"], "cpu_seconds": seconds})
    return sorted(result, key=lambda p: p["cpu_seconds"], reverse=True)


class Connection:
    def __init__(self, address, timeout=10):
        self.connection = http.client.HTTPConnection(address, timeout=timeout)

    def request(self, method, path, body=None, raw=False):
        data = None if body is None else json.dumps(body, separators=(",", ":"))
        self.connection.request(method, path, data, {"Content-Type": "application/json"})
        response = self.connection.getresponse()
        payload = response.read()
        if response.status >= 400:
            raise RuntimeError(f"HTTP {response.status}: {payload.decode()[:1000]}")
        return payload.decode() if raw else json.loads(payload)

    def close(self):
        self.connection.close()


def market(index, venue, kind):
    config = {"instrument": {"instrument_id": f"BENCH-{index}", "venue_id": venue,
                            "symbol": f"BENCH-{index}", "base_asset": "BASE", "quote_asset": "QUOTE",
                            "tick_size": 1, "lot_size": 1},
              "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {}}
    if kind == "Perp":
        config["clearing"]["leverage"] = 5
        config["initial_mark_price_tick"] = 100
    else:
        config["risk"]["allow_short"] = False
    return {kind: config}


def recipes(topology, kind, concurrency):
    """Each room has the same account population; only active routing changes.

    Identical BASE/QUOTE identifiers intentionally exercise shared venue funds
    and perp margin groups. Different venues have independent balances.
    """
    room_count = 4 if topology == "four_rooms" else 1
    market_count = 1 if topology == "one_book" else 4
    accounts = list(range(1000, 1000 + 2 * concurrency))
    payloads = []
    for room_index in range(room_count):
        indexes = [room_index] if room_count == 4 else list(range(market_count))
        markets = [market(i, f"venue-{i}" if topology == "four_venues" else "venue", kind) for i in indexes]
        payloads.append({"scenario": {"room_id": f"bench-{room_index}", "market": markets[0],
                         "extra_markets": markets[1:],
                         "accounts": [{"Spot": {"account_id": account, "cash_balance": 100000,
                                                 "position_qty": 100}} for account in accounts],
                         "seed_orders": []}, "agents": [], "autostart_agents": False})
    routes = []
    for worker in range(concurrency):
        index = 0 if topology == "one_book" else worker % 4
        room = f"bench-{index}" if room_count == 4 else "bench-0"
        routes.append({"worker": worker, "room": room, "instrument": f"BENCH-{index}",
                       "accounts": [1000 + 2 * worker, 1001 + 2 * worker]})
    return payloads, routes


def metrics(connection):
    raw = connection.request("GET", "/metrics", raw=True)
    parsed = {}
    for line in raw.splitlines():
        if line and not line.startswith("#"):
            name, value = line.rsplit(" ", 1)
            parsed[name] = float(value)
    return raw, parsed


def timed_stages(before, after):
    result = {}
    for name in ("directory_wait", "room_wait", "room_held", "order_prepare", "order_apply",
                 "order_receipt", "order_journal", "order_publish"):
        prefix = f"marketforge_bench_{name}_"
        if prefix + "count_total" not in after:
            continue
        count = after[prefix + "count_total"] - before.get(prefix + "count_total", 0)
        nanos = after[prefix + "nanos_total"] - before.get(prefix + "nanos_total", 0)
        result[name] = {"count": int(count), "total_ms": nanos / 1e6,
                        "mean_ms": nanos / count / 1e6 if count else None,
                        "process_max_ms_including_warmup": after[prefix + "max_nanos"] / 1e6}
    return result


def worker_cycles(connection, route, workload, cycles, phase, started, records):
    path = f"/rooms/{route['room']}/orders"
    a, b = route["accounts"]
    def submit(account, action, expected_event):
        begin = time.perf_counter()
        response = connection.request("POST", path, {"participant_id": f"bench-{route['worker']}",
                                      "account_id": account, "instrument_id": route["instrument"], "action": action})
        end = time.perf_counter()
        row = {"worker": route["worker"], "room": route["room"], "instrument": route["instrument"],
               "operation": next(iter(action)), "phase": phase, "started_seconds": begin - started,
               "finished_seconds": end - started, "latency_ms": (end - begin) * 1000,
               "command_seq": response["command_seq"], "accepted": response["accepted"],
               "events": response["events"], "clearing_event_count": response["clearing_event_count"]}
        records.append(row)
        if not response["accepted"] or not any(e["type"] == expected_event for e in response["events"]):
            raise RuntimeError(f"unexpected receipt {response}")
        return response
    for _ in range(cycles):
        if workload == "rest_cancel":
            response = submit(a, {"PlaceLimit": {"side": "Buy", "price_tick": 1, "qty": 1}}, "OrderRested")
            order = next(e["order_id"] for e in response["events"] if e["type"] == "OrderRested")
            submit(a, {"Cancel": {"order_id": order}}, "OrderCanceled")
        else:
            # Reverse trade restores both inventories and cash with zero fees.
            # Other exclusive account pairs may match across workers; aggregate
            # conservation and flat final positions are checked after draining.
            for seller, buyer in ((a, b), (b, a)):
                submit(seller, {"PlaceLimit": {"side": "Sell", "price_tick": 100, "qty": 1}}, "OrderRested")
                submit(buyer, {"PlaceImmediateOrCancel": {"side": "Buy", "price_tick": 100, "qty": 1}}, "OrderFilled")


def run_case(args, topology, kind, workload, concurrency, label):
    output = args.output / label
    output.mkdir(parents=True, exist_ok=False)
    address = f"127.0.0.1:{port()}"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MARKETFORGE_", "PG"))}
    env["MARKETFORGE_BIND_ADDR"] = address
    env["PATH"] = str(Path(sys.executable).parent) + os.pathsep + env.get("PATH", "")
    records, errors, resources, reads = [], [], [], []
    stop = threading.Event()
    payloads, routes = recipes(topology, kind, concurrency)
    save(output / "recipes.json", payloads)
    save(output / "routes.json", routes)
    process = None
    connection = Connection(address)
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with (output / "server.log").open("w", encoding="utf-8") as log:
        try:
            process = subprocess.Popen([str(args.server.resolve())], cwd=output.resolve(), env=env,
                                       stdout=log, stderr=subprocess.STDOUT, creationflags=flags)
            deadline = time.perf_counter() + 25
            while True:
                try:
                    connection.request("GET", "/health/ready")
                    break
                except (OSError, RuntimeError):
                    connection.close()
                    connection = Connection(address)
                    if process.poll() is not None or time.perf_counter() > deadline:
                        raise RuntimeError("isolated server readiness failed")
                    time.sleep(.05)
            for payload in payloads:
                connection.request("POST", "/rooms", payload)
            initial = {p["scenario"]["room_id"]: connection.request(
                "GET", f"/rooms/{p['scenario']['room_id']}/venue/accounts/by-venue") for p in payloads}
            warm_started = time.perf_counter()
            with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
                def warm(route):
                    client = Connection(address)
                    try:
                        worker_cycles(client, route, workload, args.warmup_cycles, "warmup", warm_started, records)
                    finally:
                        client.close()
                list(pool.map(warm, routes))
            raw, before = metrics(connection)
            (output / "metrics-before.txt").write_text(raw, encoding="utf-8")
            before_background = process_snapshot()
            cpu_begin = cpu_seconds(process)
            cpu_window_start = time.perf_counter()
            client_cpu_begin = time.process_time()
            barrier = threading.Barrier(concurrency + 1)
            cycles_each = args.cycles // concurrency
            origin = [0.0]
            def execute(route):
                client = Connection(address)
                try:
                    client.request("GET", "/health/live")
                    barrier.wait(timeout=20)
                    worker_cycles(client, route, workload, cycles_each, "measure", origin[0], records)
                except Exception as exc:
                    errors.append({"worker": route["worker"], "error": str(exc)})
                    stop.set()
                finally:
                    client.close()
            def monitor():
                while not stop.is_set():
                    snapshot = process_snapshot()
                    memory = peak_memory(process)
                    resources.append({"elapsed": time.perf_counter() - origin[0], "peak_bytes": memory,
                                      "compilers": [p for p in snapshot if p["name"].lower() in ("rustc.exe", "cargo.exe")],
                                      "server_cpu_seconds": cpu_seconds(process)})
                    if memory and memory > args.max_memory_mib * 1024**2:
                        errors.append({"kind": "memory_guard", "bytes": memory})
                        process.terminate()
                        stop.set()
                        return
                    if time.perf_counter() - origin[0] > args.max_seconds:
                        errors.append({"kind": "duration_guard"})
                        process.terminate()
                        stop.set()
                        return
                    stop.wait(.25)
            def read_probe():
                client = Connection(address)
                route = routes[0]
                try:
                    while not stop.is_set():
                        began = time.perf_counter()
                        client.request("GET", f"/rooms/{route['room']}/instruments/{route['instrument']}/book")
                        reads.append((time.perf_counter() - began) * 1000)
                        stop.wait(.05)
                except Exception as exc:
                    errors.append({"kind": "read_probe", "error": str(exc)})
                finally:
                    client.close()
            if args.load_client:
                origin[0] = time.perf_counter()
                monitoring = threading.Thread(target=monitor, daemon=True)
                monitoring.start()
                reading = threading.Thread(target=read_probe, daemon=True)
                reading.start()
                with (output / "load-client.log").open("w", encoding="utf-8") as load_log:
                    generator = subprocess.Popen([str(args.load_client.resolve()), f"http://{address}",
                        str((output / "routes.json").resolve()), workload, str(args.cycles),
                        str((output / "load-client.json").resolve())], stdout=load_log, stderr=subprocess.STDOUT,
                        creationflags=flags)
                    try:
                        generator.wait(timeout=args.max_seconds)
                        generator_cpu = cpu_seconds(generator)
                    finally:
                        if generator.poll() is None:
                            generator.terminate()
                            generator.wait(timeout=10)
                generator_result = json.loads((output / "load-client.json").read_text(encoding="utf-8"))
                records.extend(generator_result["requests"])
                errors.extend({"kind": "rust_generator", "error": e} for e in generator_result["errors"])
                elapsed = generator_result["elapsed_seconds"]
                stop.set()
                monitoring.join(timeout=3)
                reading.join(timeout=3)
            else:
                generator_cpu = None
                with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as pool:
                    futures = [pool.submit(execute, route) for route in routes]
                    origin[0] = time.perf_counter()
                    monitoring = threading.Thread(target=monitor, daemon=True)
                    monitoring.start()
                    reading = threading.Thread(target=read_probe, daemon=True)
                    reading.start()
                    barrier.wait(timeout=20)
                    for future in futures:
                        future.result(timeout=args.max_seconds)
                    elapsed = time.perf_counter() - origin[0]
                    stop.set()
                    monitoring.join(timeout=3)
                    reading.join(timeout=3)
            cpu_end = cpu_seconds(process)
            cpu_window_seconds = time.perf_counter() - cpu_window_start
            client_cpu = time.process_time() - client_cpu_begin
            after_background = process_snapshot()
            raw, after = metrics(connection)
            (output / "metrics-after.txt").write_text(raw, encoding="utf-8")
            final = {p["scenario"]["room_id"]: connection.request(
                "GET", f"/rooms/{p['scenario']['room_id']}/venue/accounts/by-venue") for p in payloads}
            books = {f"{route['room']}/{route['instrument']}": connection.request(
                "GET", f"/rooms/{route['room']}/instruments/{route['instrument']}/book") for route in routes}
            save(output / "initial-accounts.json", initial)
            save(output / "final-accounts.json", final)
            save(output / "final-books.json", books)
            measured = [r for r in records if r["phase"] == "measure"]
            per_operation = {name: percentiles([r["latency_ms"] for r in measured if r["operation"] == name])
                             for name in sorted({r["operation"] for r in measured})}
            commands = args.cycles * (2 if workload == "rest_cancel" else 4)
            ids = [event["order_id"] for r in records for event in r["events"] if event["type"] == "OrderRested"]
            accepted_ids = [event["order_id"] for r in records for event in r["events"] if event["type"] == "OrderAccepted"]
            sequences = {(r["room"], r["command_seq"]) for r in records}
            # Matching across account pairs may redistribute holdings. Require
            # full equality for cancel-only, asset conservation for fills.
            def aggregate(value):
                totals = {}
                def walk(node):
                    if isinstance(node, dict):
                        if "asset_id" in node and "total" in node:
                            asset = node["asset_id"]
                            totals[asset] = totals.get(asset, 0) + int(node["total"])
                            if int(node.get("reserved", 0)) != 0:
                                raise RuntimeError("unreleased final reservation")
                        else:
                            for item in node.values(): walk(item)
                    elif isinstance(node, list):
                        for item in node: walk(item)
                walk(value)
                return totals
            conserved = aggregate(initial) == aggregate(final)
            validation = {"expected_commands": commands, "completed_commands": len(measured),
                          "all_accepted": all(r["accepted"] for r in measured),
                          "unique_rested_order_ids": len(ids) == len(set(ids)),
                          "unique_all_accepted_order_ids": len(accepted_ids) == len(set(accepted_ids)),
                          "unique_room_command_sequences": len(sequences) == len(records),
                          "asset_conservation": conserved,
                          "accounts_exactly_restored": initial == final,
                          "empty_final_books": all(not book["bids"] and not book["asks"] for book in books.values()),
                          "filled_orders": sum(e["type"] == "OrderFilled" for r in measured for e in r["events"]),
                          "trades": sum(e["type"] == "TradePrinted" for r in measured for e in r["events"]),
                          "clearing_events": sum(r["clearing_event_count"] for r in measured)}
            if len(measured) != commands or not validation["all_accepted"] or not conserved:
                errors.append({"kind": "validation", "receipt": validation})
            if (not validation["unique_rested_order_ids"] or not validation["unique_room_command_sequences"]
                    or not validation["unique_all_accepted_order_ids"]):
                errors.append({"kind": "identity_validation"})
            if not validation["empty_final_books"]:
                errors.append({"kind": "resting_orders_remain"})
            if workload == "rest_cancel" and initial != final:
                errors.append({"kind": "balance_restoration"})
            if workload == "roundtrip" and validation["trades"] != 2 * args.cycles:
                errors.append({"kind": "fill_count"})
            results = {"label": label, "topology": topology, "kind": kind, "workload": workload,
                       "concurrency": concurrency, "cycles": args.cycles, "warmup_cycles_per_worker": args.warmup_cycles,
                       "elapsed_seconds": elapsed, "accepted_commands_per_second": len(measured) / elapsed,
                       "http_latency": percentiles([r["latency_ms"] for r in measured]), "latency_by_operation": per_operation,
                       "read_probe_latency": percentiles(reads),
                       "server_cpu_seconds": cpu_end - cpu_begin if cpu_end is not None else None,
                       "cpu_window_seconds": cpu_window_seconds,
                       "server_average_cores": (cpu_end - cpu_begin) / cpu_window_seconds if cpu_end is not None else None,
                       "client_cpu_seconds": client_cpu, "client_average_cores": client_cpu / cpu_window_seconds,
                       "rust_generator_cpu_seconds": generator_cpu,
                       "load_generator": "Rust async" if args.load_client else "Python thread per connection",
                       "peak_memory_mib": peak_memory(process) / 1024**2,
                       "stages": timed_stages(before, after), "validation": validation,
                       "background_cpu": background_delta(before_background, after_background, {os.getpid(), process.pid}),
                       "compiler_overlap_samples": sum(bool(r["compilers"]) for r in resources),
                       "resources": resources, "errors": errors,
                       "server_sha256": hashlib.sha256(args.server.read_bytes()).hexdigest(),
                       "storage": "in-memory journal, single shared journal worker; not PostgreSQL",
                       "latency_definition": "closed-loop, all measured successful responses; failures reported separately",
                       "limits": "topology changes state size/history distribution as well as lock count; not a pure lock ablation"}
            save(output / "requests.json", records)
            save(output / "report.json", results)
            print(json.dumps({k: results[k] for k in ("label", "accepted_commands_per_second", "http_latency",
                             "server_average_cores", "peak_memory_mib", "compiler_overlap_samples", "errors")}), flush=True)
            return results
        except Exception as exc:
            save(output / "failed.json", {"error": repr(exc), "errors": errors, "requests": records})
            raise
        finally:
            stop.set()
            connection.close()
            if process and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--load-client", type=Path, help="use the compiled Rust async generator for formal load")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--topologies", nargs="+", choices=["one_book", "four_books", "four_rooms", "four_venues"],
                        default=["one_book", "four_books", "four_rooms"])
    parser.add_argument("--kinds", nargs="+", choices=["Spot", "Perp"], default=["Spot"])
    parser.add_argument("--workloads", nargs="+", choices=["rest_cancel", "roundtrip"], default=["rest_cancel", "roundtrip"])
    parser.add_argument("--concurrency", type=int, nargs="+", default=[1, 8, 32])
    parser.add_argument("--cycles", type=int, default=1024)
    parser.add_argument("--warmup-cycles", type=int, default=2)
    parser.add_argument("--repeats", type=int, default=2)
    parser.add_argument("--max-memory-mib", type=int, default=2048)
    parser.add_argument("--max-seconds", type=int, default=120)
    parser.add_argument("--allow-compiler-overlap", action="store_true",
                        help="smoke checks only; such results must not be used for performance comparisons")
    args = parser.parse_args()
    if not 1 <= args.cycles <= 8192 or not 1 <= args.repeats <= 4:
        parser.error("bounded cycles 1..8192 and repeats 1..4 required")
    if any(not 1 <= c <= 64 or args.cycles % c for c in args.concurrency):
        parser.error("concurrency 1..64 must divide cycles")
    args.output.mkdir(parents=True, exist_ok=True)
    matrix = [(topology, kind, workload, c) for kind in args.kinds for workload in args.workloads
              for c in args.concurrency for topology in args.topologies]
    reports = []
    for repeat in range(args.repeats):
        cases = matrix if repeat % 2 == 0 else list(reversed(matrix))
        for topology, kind, workload, concurrency in cases:
            # Avoid known compiler interference. Do not stop other tasks.
            waiting = time.monotonic()
            last_notice = 0
            while not args.allow_compiler_overlap and any(
                    p["name"].lower() in ("cargo.exe", "rustc.exe") for p in process_snapshot()):
                if time.monotonic() - waiting > 180:
                    raise RuntimeError("compilers still active; restart benchmark after compilation")
                if time.monotonic() - last_notice > 30:
                    print("waiting for existing compilation to finish", flush=True)
                    last_notice = time.monotonic()
                time.sleep(5)
            label = f"r{repeat + 1}-{kind.lower()}-{workload}-{topology}-c{concurrency}"
            reports.append(run_case(args, topology, kind, workload, concurrency, label))
            save(args.output / "report.json", {"cases": reports, "scope": "bounded fixed-work local synthetic HTTP load"})
    if any(r["errors"] for r in reports):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
