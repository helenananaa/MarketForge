"""Run the ignored fixed-order native delivery benchmark in ABBA order.

Build first: cargo test -p exchange-server --release --lib
  fixed_native_delivery_workload --no-run
The two modes use the same Release test binary; receipts must be identical.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess
import time

from benchmark_market_load import cpu_seconds
from validate_behavior_market_live import peak_memory


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path, required=True)
    parser.add_argument("--recipe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--steps", type=int, default=64)
    parser.add_argument("--rounds", type=int, default=2)
    args = parser.parse_args()
    if not 1 <= args.steps <= 128 or not 1 <= args.rounds <= 4:
        parser.error("steps must be 1..128 and rounds 1..4")
    args.output.mkdir(parents=True, exist_ok=True)
    results = []
    for index, mode in enumerate(["owned", "native", "native", "owned"] * args.rounds):
        output = args.output / f"{index + 1:02}-{mode}.json"
        env = dict(os.environ, MF_FIXED_RECIPE=str(args.recipe.resolve()),
                   MF_FIXED_OUTPUT=str(output.resolve()), MF_DELIVERY_MODE=mode,
                   MF_FIXED_STEPS=str(args.steps))
        with output.with_suffix(".log").open("wb") as log:
            child = subprocess.Popen([str(args.test_binary.resolve()), "fixed_native_delivery_workload",
                                      "--ignored", "--nocapture", "--test-threads=1"],
                                     env=env, stdout=log, stderr=subprocess.STDOUT)
            peak = None
            cpu = None
            deadline = time.monotonic() + 300
            while child.poll() is None:
                sample = peak_memory(child)
                if sample is not None:
                    peak = max(peak or 0, sample)
                cpu = cpu_seconds(child)
                if time.monotonic() >= deadline:
                    child.terminate()
                    child.wait(timeout=10)
                    raise TimeoutError(f"fixed workload exceeded 300s: {output}")
                time.sleep(.05)
            if child.returncode:
                raise RuntimeError(f"fixed workload exited {child.returncode}: {output.with_suffix('.log')}")
        report = json.loads(output.read_text(encoding="utf-8"))
        report.update(peak_memory_mib=None if peak is None else peak / 1024**2,
                      sampled_cpu_seconds=cpu)
        output.write_text(json.dumps(report, indent=2), encoding="utf-8")
        results.append(report)
        print(json.dumps(report), flush=True)
    assert len({r["receipt_sha256"] for r in results}) == 1, "state/execution receipts differ"
    assert len({(r["decisions"], r["actions"], r["empty"]) for r in results}) == 1
    summary = {"test_binary_sha256": hashlib.sha256(args.test_binary.read_bytes()).hexdigest(),
               "recipe_sha256": hashlib.sha256(args.recipe.read_bytes()).hexdigest(),
               "receipt_equal": True, "cases": results, "medians": {}}
    for mode in ("owned", "native"):
        subset = [r for r in results if r["mode"] == mode]
        summary["medians"][mode] = {
            key: statistics.median(r[key] for r in subset if r[key] is not None)
            for key in ("elapsed_ms", "delivery_ms", "peak_memory_mib", "sampled_cpu_seconds")
            if any(r[key] is not None for r in subset)}
    summary["qualification"] = "fixed-order seeded workload through the normal gateway and memory journal; elapsed/delivery exclude final receipt serialization, sampled process CPU/peak include it; not a production capacity claim"
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
