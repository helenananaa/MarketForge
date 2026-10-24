"""Differential qualification of exact installed Pine sources (no admission writes).

Compare schema-2 responses, or all economic fields for schema-4 replay state,
with the installed Python broker. Exercise cached continuation, cold recovery,
duplicate calls, rejected orders and optional legacy-state migration.
Trading receipts from the real gateway are a separate required acceptance gate.
"""
import argparse
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import random
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python/tests"))
from test_pine_bot import bot
from test_pine_broker import broker_request, perp, ack
from pine_receipt_projection import trading_projection


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, default=ROOT / "exchange-server/src/pine_inprocess/ecosystem-qualified.json")
    parser.add_argument("--bars", type=int, default=256)
    parser.add_argument("--replay-state", action="store_true")
    parser.add_argument("--check-migration", action="store_true",
                        help="also convert the Python state at bar 129 and retry from disk")
    parser.add_argument("--check-errors", action="store_true",
                        help="compare exact interpreter errors and retry from the last committed state")
    parser.add_argument("--timeout-ms", type=int, default=30000)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=False)
    sources = json.loads(args.manifest.read_text(encoding="utf-8"))
    process = subprocess.Popen([str(args.probe), str(ROOT / "bot-plugins/pine-strategy")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=(args.output / "probe.log").open("w"),
        text=True, encoding="utf-8", env={**os.environ,
            "MARKETFORGE_PINE_REPLAY_STATE":"1" if args.replay_state else "0"})
    result = {"scripts": [], "probe_sha256": hashlib.sha256(args.probe.read_bytes()).hexdigest(),
              "manifest_sha256": hashlib.sha256(args.manifest.read_bytes()).hexdigest(),
              "replay_state":args.replay_state, "check_migration":args.check_migration,
              "timeout_ms":args.timeout_ms}
    import pine_compat
    result["python_runtime"] = {"version": pine_compat.__version__,
        "sha256": hashlib.sha256(Path(pine_compat.__file__).read_bytes()).hexdigest()}
    result["adapter_sha256"] = {name: hashlib.sha256((ROOT / "bot-plugins/pine-strategy" / name).read_bytes()).hexdigest()
        for name in ("bot.py", "broker.py", "incremental.py", "incremental-qualified.json")}
    def rust(req, reset):
        process.stdin.write(json.dumps({"request": req, "reset": reset, "timeout_ms": args.timeout_ms}) + "\n")
        process.stdin.flush()
        line = process.stdout.readline()
        if not line:
            raise RuntimeError("probe exited; inspect probe.log")
        return json.loads(line)
    try:
        for number, (name, digest) in enumerate(sources.items()):
            started = time.perf_counter()
            source = bot.load_source(name)
            if not bot.incremental_module().qualified(source):
                raise ValueError(f"source lacks independent incremental qualification: {name}")
            if hashlib.sha256(source.encode()).hexdigest() != digest:
                raise ValueError(f"source hash mismatch: {name}")
            row = {"script": name, "sha256": digest, "comparisons": 0, "intents": 0, "actions": 0, "passed": True}
            try:
                for seed in (7, 19):
                    rng = random.Random(seed)
                    prices = [100000 + int(3000 * math.sin(i / 8) + 1500 * math.sin(i / 31)) + rng.randrange(-400, 401) for i in range(args.bars)]
                    previous = previous_rust = None
                    points = (1,31,32,33,64,65,128,129,256,512,1024,2048,args.bars)
                    for end in sorted({n for n in points if n <= args.bars}):
                        req = perp(broker_request(prices[:end]), position=0 if seed == 7 else 7,
                                   avg=0 if seed == 7 else prices[0], cash=100000000)
                        req["config"].update(script=name, quantize_strategy_orders=True, history_limit=2048, max_qty=10, inventory_cap=100)
                        req["participant"]["participant_id"] = f"qualification-{number}-{seed}"
                        req["observation"]["instrument"] = {"tick_size": 1, "lot_size": 1}
                        req["observation"]["bot_market_data"]["private_history_complete"] = True
                        rust_req = copy.deepcopy(req)
                        if previous is not None:
                            ack(req, previous)
                            ack(rust_req, previous_rust)
                            for receipt in req["state"]["_host_receipts"]:
                                receipt.update(accepted=False, order_id=None, error="qualification rejection")
                            rust_req["state"]["_host_receipts"] = copy.deepcopy(req["state"]["_host_receipts"])
                        # Production requests pass through serde_json's sorted
                        # object maps, including persisted exit insertion order.
                        req = json.loads(json.dumps(req, sort_keys=True))
                        rust_req = json.loads(json.dumps(rust_req, sort_keys=True))
                        try:
                            expected = bot.decide(copy.deepcopy(req))
                        except Exception as error:
                            if not args.check_errors:
                                raise
                            # An invalid strategy value must still fail closed,
                            # both in a cached session and after cold recovery.
                            for reset in (False, True):
                                failed = rust(rust_req, reset)
                                raw_error = failed.get("error")
                                rust_error = raw_error
                                # The Rust probe formats the runtime error's
                                # typed wrapper; Python exposes its message.
                                prefix = 'RuntimeError { message: '
                                if isinstance(rust_error, str) and rust_error.startswith(prefix) and rust_error.endswith(' }'):
                                    rust_error = json.loads(rust_error[len(prefix):-2])
                                if rust_error != str(error) or "response" in failed:
                                    raise AssertionError(f"error mismatch: Python={error}; Rust={failed}") from error
                                row["comparisons"] += 1
                            row.setdefault("matched_errors", []).append({"seed":seed, "bar":end, "error":str(error), "rust_error":raw_error})
                            continue
                        actual = rust(rust_req, reset=end in (1,33,128,129,512))
                        reference = trading_projection(expected) if args.replay_state else expected
                        response = actual.get("response")
                        compared = trading_projection(response) if args.replay_state and response else response
                        if compared != reference:
                            (args.output / f"failure-{number}.json").write_text(json.dumps({"request": req, "expected": expected, "actual": actual}, indent=2), encoding="utf-8")
                            raise AssertionError(actual.get("error", "response mismatch"))
                        row["comparisons"] += 1
                        row["intents"] += len(expected["state"].get("last_intents", []))
                        row["actions"] += len(expected["actions"])
                        if args.replay_state and args.check_migration and end == 129:
                            migrated = rust(req, reset=True)
                            if "response" not in migrated or trading_projection(migrated["response"]) != reference:
                                raise AssertionError(migrated.get("error", "legacy migration mismatch"))
                            if migrated["response"]["state"]["broker_schema"] != 4 or rust(req, reset=True) != migrated:
                                raise AssertionError("legacy migration restart mismatch")
                            row["comparisons"] += 2
                        if end in (65, args.bars):
                            for reset in (False, True):
                                retry = rust(rust_req, reset)
                                if retry.get("response") != response:
                                    raise AssertionError("speculative retry or reconstruction mismatch")
                                row["comparisons"] += 1
                        previous = expected
                        previous_rust = response
            except Exception as error:
                row.update(passed=False, error=str(error))
            row["seconds"] = time.perf_counter() - started
            result["scripts"].append(row)
            (args.output / "summary.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
            print(json.dumps(row), flush=True)
    finally:
        process.stdin.close()
        process.wait(timeout=15)
        process.stdout.close()
    return int(any(not r["passed"] for r in result["scripts"]))


if __name__ == "__main__":
    raise SystemExit(main())
