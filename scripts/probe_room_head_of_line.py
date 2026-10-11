"""Controlled head-of-line probe: one slow manual clock decision, two markets."""
import argparse
import concurrent.futures
import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from benchmark_room_contention import Connection, recipes, metrics, timed_stages
from validate_behavior_market_live import port, save


def run(server, output, repeat):
    output = output / f"trial-{repeat}"
    output.mkdir(parents=True, exist_ok=False)
    plugin = output / "plugins" / "slow"
    plugin.mkdir(parents=True)
    marker = plugin / "entered.txt"
    manifest = {"bot": {"id": "bench.slow", "name": "Bounded manual-step diagnostic",
                         "version": "1.0.0", "protocol_version": "bot.v1", "state_version": 1,
                         "runtime": "process", "parameters": {}},
                "command": [sys.executable, "slow.py"], "timeout_ms": 3000}
    save(plugin / "bot.json", manifest)
    (plugin / "slow.py").write_text(
        "import json, sys, time\nfrom pathlib import Path\n"
        "request=json.loads(sys.stdin.readline())\n"
        "Path('entered.txt').write_text('entered')\ntime.sleep(.75)\n"
        "print(json.dumps({key:request[key] for key in ('protocol_version','plugin_id','plugin_version','state_version')} | "
        "{'actions':[], 'state':{'done':True}}), flush=True)\n", encoding="utf-8")
    payloads, routes = recipes("four_books", "Spot", 4)
    scenario = payloads[0]
    scenario["agents"] = [{"Plugin": {"participant": {"participant_id": "slow", "kind": "RuleAgent",
                                  "room_id": "bench-0", "account_id": 1000, "instrument_id": "BENCH-0"},
                           "plugin_id": "bench.slow", "plugin_version": "1.0.0", "state_version": 1,
                           "config_version": 1, "config": {}, "seed": 7}}]
    other = copy.deepcopy(scenario)
    other["scenario"]["room_id"] = "bench-other"
    other["agents"] = []
    address = f"127.0.0.1:{port()}"
    env = {k: v for k, v in os.environ.items() if not k.startswith(("MARKETFORGE_", "PG"))}
    env.update(MARKETFORGE_BIND_ADDR=address, MARKETFORGE_BOT_PLUGIN_DIR=str(plugin.parent.resolve()))
    flags = subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0
    with (output / "server.log").open("w", encoding="utf-8") as log:
        process = subprocess.Popen([str(server.resolve())], cwd=output.resolve(), env=env,
                                   stdout=log, stderr=subprocess.STDOUT, creationflags=flags)
        client = Connection(address)
        try:
            deadline = time.monotonic() + 20
            while True:
                try:
                    client.request("GET", "/health/ready")
                    break
                except (OSError, RuntimeError):
                    client.close()
                    client = Connection(address)
                    if process.poll() is not None or time.monotonic() > deadline:
                        raise RuntimeError("readiness failed")
                    time.sleep(.05)
            client.request("POST", "/rooms", scenario)
            client.request("POST", "/rooms", other)
            client.request("POST", "/rooms/bench-0/pause", {})
            raw, before = metrics(client)
            (output / "metrics-before.txt").write_text(raw, encoding="utf-8")
            def timed(method, path, body=None):
                conn = Connection(address)
                begun = time.perf_counter()
                try:
                    response = conn.request(method, path, body)
                    return {"latency_ms": (time.perf_counter() - begun) * 1000, "response": response}
                finally:
                    conn.close()
            with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
                slow = pool.submit(timed, "POST", "/rooms/bench-0/clock/step", {})
                deadline = time.monotonic() + 5
                while not marker.exists():
                    if slow.done(): slow.result(); raise RuntimeError("manual step completed without entering plugin")
                    if time.monotonic() > deadline: raise RuntimeError("plugin did not enter")
                    time.sleep(.005)
                order = {"participant_id": "probe", "instrument_id": "BENCH-1", "account_id": 1002,
                         "action": {"PlaceLimit": {"side": "Buy", "price_tick": 1, "qty": 1}}}
                same = pool.submit(timed, "POST", "/rooms/bench-0/orders", order)
                outside = pool.submit(timed, "POST", "/rooms/bench-other/orders", order)
                read = pool.submit(timed, "GET", "/rooms/bench-0/instruments/BENCH-1/book")
                result = {"slow_manual_step": slow.result(), "other_instrument_same_room": same.result(),
                          "same_instrument_other_room": outside.result(), "same_room_committed_read": read.result(),
                          "scope": "controlled 750ms manual-step plugin; not typical automatic bot computation"}
            raw, after = metrics(client)
            (output / "metrics-after.txt").write_text(raw, encoding="utf-8")
            result["stages"] = timed_stages(before, after)
            result["paused_order_rejected_as_expected"] = (
                not result["other_instrument_same_room"]["response"]["accepted"] and
                result["other_instrument_same_room"]["response"]["reject_reason"] == "market paused")
            result["verified"] = (result["paused_order_rejected_as_expected"] and
                                  result["same_instrument_other_room"]["response"]["accepted"] and
                                  result["other_instrument_same_room"]["latency_ms"] > 500 and
                                  result["same_instrument_other_room"]["latency_ms"] < 250 and
                                  result["same_room_committed_read"]["latency_ms"] < 250)
            save(output / "report.json", result)
            print(json.dumps({key: value.get("latency_ms") if isinstance(value, dict) else value
                              for key, value in result.items() if key != "stages"}), flush=True)
            if not result["verified"]: raise RuntimeError("head-of-line probe did not meet its conditions")
            return result
        finally:
            client.close()
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    results = [run(args.server, args.output, i) for i in range(1, 4)]
    save(args.output / "report.json", {"trials": results})


if __name__ == "__main__":
    main()
