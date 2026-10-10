"""Deterministic equal-progress HTTP/heap comparison; not live-worker capacity."""
import argparse
import ctypes
from ctypes import wintypes
import hashlib
import http.client
import json
import os
from pathlib import Path
import statistics
import subprocess
import threading
import time

from benchmark_market_load import load_recipe, percentiles
from validate_behavior_market_live import port


def canonical(value):
    if isinstance(value, dict):
        return {k: sorted(v) if k == "seen_order_ids" else canonical(v) for k, v in value.items()}
    if isinstance(value, list):
        return [canonical(v) for v in value]
    return value


def digest(value):
    return hashlib.sha256(json.dumps(canonical(value), sort_keys=True, separators=(",", ":")).encode()).hexdigest()


class Memory(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("PageFaultCount", wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in ("peak_working_set", "working_set", "quota_peak_paged", "quota_paged",
            "quota_peak_nonpaged", "quota_nonpaged", "private_bytes", "peak_private_bytes", "private_usage")]


def memory(process):
    counters = Memory()
    counters.cb = ctypes.sizeof(counters)
    call = ctypes.windll.psapi.GetProcessMemoryInfo
    call.argtypes = [wintypes.HANDLE, ctypes.POINTER(Memory), wintypes.DWORD]
    call.restype = wintypes.BOOL
    if not call(wintypes.HANDLE(int(process._handle)), ctypes.byref(counters), counters.cb):
        raise ctypes.WinError()
    return {key: getattr(counters, key) for key in ["working_set", "peak_working_set", "private_bytes", "peak_private_bytes"]}


def request(bind, method, path, body=None, connection=None):
    conn = connection or http.client.HTTPConnection(bind, timeout=120)
    payload = None if body is None else json.dumps(body).encode()
    started = time.perf_counter()
    try:
        if conn.sock is None: conn.connect()
        connected = time.perf_counter()
        conn.request(method, path, body=payload, headers={"Content-Type": "application/json"})
        response = conn.getresponse()
        headers = time.perf_counter()
        raw = response.read()
        read = time.perf_counter()
        if response.status != 200:
            raise RuntimeError(f"{method} {path}: {response.status}: {raw[:500]!r}")
        value = json.loads(raw)
        parsed = time.perf_counter()
        return value, {"total_ms": (parsed-started)*1000, "raw_ms": (read-started)*1000,
            "connect_ms": (connected-started)*1000, "headers_ms": (headers-connected)*1000,
            "body_ms": (read-headers)*1000, "parse_ms": (parsed-read)*1000, "bytes": len(raw)}
    finally:
        if connection is None: conn.close()


def run(args, binary, output):
    output.mkdir(parents=True, exist_ok=False)
    bind = f"127.0.0.1:{port()}"
    spec = load_recipe(args.bots, room="equal-progress", seed=7,
        template=json.loads(args.template.read_text(encoding="utf-8")))
    spec["autostart_agents"] = False
    (output/"recipe.json").write_text(json.dumps(spec, indent=2), encoding="utf-8")
    env = {k:v for k,v in os.environ.items() if not k.startswith(("MARKETFORGE_", "PG"))}
    with (output/"server.log").open("w", encoding="utf-8") as log:
        process = subprocess.Popen([str(binary), bind], cwd=output, env=env, stdout=log, stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
        points = []
        observations = []
        stop = threading.Event()
        errors = []
        def sample():
            while not stop.wait(.05):
                try: observations.append({"time":time.perf_counter(), **memory(process)})
                except Exception as error: errors.append(str(error)); return
        worker = None
        try:
            deadline = time.perf_counter()+30
            while True:
                try: request(bind,"GET","/health/ready"); break
                except (OSError, RuntimeError):
                    if process.poll() is not None or time.perf_counter()>deadline: raise RuntimeError("readiness failed")
                    time.sleep(.05)
            request(bind,"POST","/rooms",spec)
            request(bind,"POST","/rooms/equal-progress/pause",{})
            request(bind,"POST","/__benchmark/start",{})
            worker = threading.Thread(target=sample,daemon=True); worker.start()
            chain = hashlib.sha256()
            total_step_ms = 0
            last_step = 0
            for target in [0, *args.steps]:
                reset, _ = request(bind,"POST","/__benchmark/reset-peak",{})
                step_ms = []
                busy = []
                # The same sequenced steps imply the same decisions/actions,
                # unlike wall-driven worker runs with asynchronous completion.
                for step in range(last_step,target):
                    if step >= max(last_step, target-20):
                        result = []; failures = []
                        def advance():
                            try: result.append(request(bind,"POST","/__benchmark/step",{}))
                            except Exception as error: failures.append(error)
                        advancing = threading.Thread(target=advance); advancing.start()
                        time.sleep(.003)
                        _, probe = request(bind,"GET","/rooms/equal-progress/view")
                        phases,_ = request(bind,"GET","/__benchmark/view-phases")
                        assert len(phases) == 1, phases
                        probe.update(auth_ms=phases[0][0]/1000, build_ms=phases[0][1]/1000)
                        busy.append(probe)
                        advancing.join()
                        if failures: raise failures[0]
                        state,timing = result[0]
                    else:
                        state, timing = request(bind,"POST","/__benchmark/step",{})
                    step_ms.append(timing["total_ms"])
                    chain.update(bytes.fromhex(digest(state)))
                total_step_ms += sum(step_ms)
                interval_heap, _ = request(bind,"GET","/__benchmark/heap")
                clock,_ = request(bind,"GET","/rooms/equal-progress/clock")
                assert clock["clock"]["step"] == target, clock
                scheduler,_ = request(bind,"GET","/rooms/equal-progress/bots")
                snapshot,_ = request(bind,"GET","/__benchmark/state")
                snapshot_hash = digest(snapshot)
                del snapshot
                storage,_ = request(bind,"GET","/__benchmark/storage")
                views = {}
                for instrument, keepalive in [(None,False),("V-BTC-PERP",False),(None,True),("V-BTC-PERP",True)]:
                    path = "/rooms/equal-progress/view" if instrument is None else f"/rooms/equal-progress/instruments/{instrument}/view"
                    values = []
                    view_hash = None
                    connection = http.client.HTTPConnection(bind, timeout=120) if keepalive else None
                    for _ in range(args.view_samples+3):
                        view,timing = request(bind,"GET",path,connection=connection)
                        phases,_ = request(bind,"GET","/__benchmark/view-phases")
                        assert len(phases) == 1, phases
                        timing.update(auth_ms=phases[0][0]/1000, build_ms=phases[0][1]/1000)
                        current = digest(view)
                        if view_hash is not None: assert current == view_hash
                        view_hash = current
                        values.append(timing)
                    rows = values[3:]
                    if connection is not None: connection.close()
                    name = (instrument or "primary") + ("-keepalive" if keepalive else "")
                    views[name] = {"sha256":view_hash,"samples":rows,
                        "percentiles":{key:percentiles([v[key] for v in rows]) for key in rows[0] if key != "bytes"},
                        "response_bytes":rows[0]["bytes"]}
                clocks = [request(bind,"GET","/rooms/equal-progress/clock")[1] for _ in range(args.view_samples)]
                heaps = [request(bind,"GET","/__benchmark/heap")[0] for _ in range(5)]
                point = {"step":target,"scheduler_sha256":digest(scheduler),"clock_sha256":digest(clock),"state_sha256":snapshot_hash,"storage":storage,
                    "busy_view_samples":busy,"busy_view_percentiles":{key:percentiles([v[key] for v in busy]) for key in busy[0] if key != "bytes"} if busy else {},
                    "step_chain_sha256":chain.hexdigest(),"interval_step_ms":sum(step_ms),"total_step_ms":total_step_ms,
                    "step_latency":percentiles(step_ms),"heap_interval":interval_heap,
                    "live_requested_bytes":statistics.median(h["live_requested_bytes"] for h in heaps),
                    "views":views,"clock_latency":percentiles([v["total_ms"] for v in clocks]),"os_memory":memory(process),
                    "scheduler_bytes":len(json.dumps(scheduler,separators=(",",":")).encode())}
                points.append(point)
                (output/"points.json").write_text(json.dumps(points,indent=2),encoding="utf-8")
                print(f"{output.name} step={target}: heap {point['live_requested_bytes']/2**20:.2f} MiB; WS {point['os_memory']['working_set']/2**20:.2f} MiB; view p95 {views['primary']['percentiles']['total_ms']['p95_ms']:.2f} ms",flush=True)
                last_step = target
            (output/"memory-samples.json").write_text(json.dumps({"samples":observations,"errors":errors}),encoding="utf-8")
            assert not errors, errors
            return points
        finally:
            stop.set()
            if worker is not None: worker.join(timeout=2)
            if process.poll() is None:
                process.terminate()
                try: process.wait(timeout=10)
                except subprocess.TimeoutExpired: process.kill();process.wait(timeout=5)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before",type=Path,required=True)
    parser.add_argument("--after",type=Path,required=True)
    parser.add_argument("--template",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--bots",type=int,default=1000)
    parser.add_argument("--steps",type=int,nargs="+",default=[100,300,700])
    parser.add_argument("--view-samples",type=int,default=40)
    parser.add_argument("--blocks",type=int,default=1)
    parser.add_argument("--resume",action="store_true",help="Reuse only complete verified runs with identical parameters and binary hashes")
    args=parser.parse_args()
    assert os.name == "nt", "OS memory counters require Windows"
    assert args.steps == sorted(set(args.steps)) and args.steps[0]>0 and args.blocks>0 and args.view_samples>=20
    args.output=args.output.resolve();args.output.mkdir(parents=True,exist_ok=True)
    bins={"before":args.before.resolve(),"after":args.after.resolve()}
    result={"definition":"real realtime Work Clock/Bots commits and observations/cache; round-robin 64 bots per step, fixed completion order; counted-System diagnostic allocator; fresh-connection/keepalive quiescent and concurrent view/memory; not asynchronous live-worker capacity", "runs":[],
        "parameters":{"bots":args.bots,"decisions_per_step":64,"steps":args.steps,"view_samples":args.view_samples,"blocks":args.blocks,"busy_probes_per_interval":20,"busy_probe_delay_ms":3},
        "binaries":{k:{"path":str(v),"sha256":hashlib.sha256(v.read_bytes()).hexdigest()} for k,v in bins.items()}}
    expected = None
    if args.resume:
        prior = json.loads((args.output/"comparison.json").read_text(encoding="utf-8"))
        assert prior["parameters"] == result["parameters"] and prior["binaries"] == result["binaries"]
        result = prior
    for index,variant in enumerate(["before","after","after","before"]*args.blocks):
        if index < len(result["runs"]):
            saved = result["runs"][index]
            assert saved["index"] == index and saved["variant"] == variant
            points = saved["points"]
        else:
            directory = args.output/f"{index:02d}-{variant}"
            attempt = 0
            while directory.exists():
                attempt += 1
                directory = args.output/f"{index:02d}-{variant}-retry-{attempt}"
            points=run(args,bins[variant],directory)
        identity=[{k:p[k] for k in ["step","scheduler_sha256","clock_sha256","state_sha256","step_chain_sha256","storage"]}|
            {"views":{name:view["sha256"] for name,view in p["views"].items()}} for p in points]
        if expected is None: expected=identity
        if identity != expected: raise RuntimeError(f"equal-progress business state differs: {variant}; receipts preserved")
        if index >= len(result["runs"]):
            result["runs"].append({"index":index,"variant":variant,"directory":directory.name,"points":points})
        (args.output/"comparison.json").write_text(json.dumps(result,indent=2),encoding="utf-8")
    print("All scheduler/clock/view/checkpoint step chains equal.",flush=True)


if __name__ == "__main__": main()
