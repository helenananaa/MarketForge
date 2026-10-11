"""Isolated ABBA bot-journal capacity comparison with test-only heap attribution.

Build: cargo test -p exchange-server --release --lib --features memory-diagnostics --no-run
Reference mode retains the original serde buffer capacity. Both modes keep every
record and use the same native dispatch, gateway, bot state and checkpoint cadence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--test-binary", type=Path, required=True)
    parser.add_argument("--recipe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--workload", choices=["fixed", "live"], default="fixed")
    parser.add_argument("--rounds", type=int, default=2)
    parser.add_argument("--steps", type=int, default=64)
    parser.add_argument("--seconds", type=int, default=30)
    args = parser.parse_args()
    if not 1 <= args.rounds <= 4:
        parser.error("rounds must be 1..4")
    args.output.mkdir(parents=True, exist_ok=True)
    results = []
    for index, capacity in enumerate(["original", "tight", "tight", "original"] * args.rounds):
        output = (args.output / f"{index + 1:02}-{capacity}.json").resolve()
        env = dict(os.environ, MF_FIXED_RECIPE=str(args.recipe.resolve()),
                   MF_BOT_JOURNAL_CAPACITY=capacity, MF_FIXED_OUTPUT=str(output),
                   MF_MEMORY_OUTPUT=str(output), MF_DELIVERY_MODE="native",
                   MF_FIXED_STEPS=str(args.steps), MF_MEMORY_SECONDS=str(args.seconds))
        test = "fixed_native_delivery_workload" if args.workload == "fixed" else "native_bot_memory_ownership_probe"
        with output.with_suffix(".log").open("wb") as log:
            subprocess.run([str(args.test_binary.resolve()), test, "--ignored", "--nocapture", "--test-threads=1"],
                           env=env, stdout=log, stderr=subprocess.STDOUT, check=True, timeout=180)
        report = json.loads(output.read_text(encoding="utf-8"))
        memory = json.loads(Path(str(output) + ".memory.json").read_text(encoding="utf-8")) if args.workload == "fixed" else report
        result = {"capacity": capacity, "report": report, "memory": memory}
        results.append(result)
        heap = memory["heap_before_receipt"] if args.workload == "fixed" else memory["samples"][-1]["heap"]
        print(json.dumps({"case": index + 1, "capacity": capacity, "heap_mib": heap["requested_live_bytes"] / 1024**2,
                          "elapsed_ms": report.get("elapsed_ms"), "step": report.get("samples", [{}])[-1].get("step")}), flush=True)
    if args.workload == "fixed":
        assert len({r["report"]["receipt_sha256"] for r in results}) == 1, "state/execution mismatch"
        assert len({(r["report"]["decisions"], r["report"]["actions"], r["report"]["empty"]) for r in results}) == 1
    medians = {}
    for capacity in ("original", "tight"):
        subset = [r for r in results if r["capacity"] == capacity]
        medians[capacity] = {"heap_mib": statistics.median(
            (r["memory"]["heap_before_receipt"] if args.workload == "fixed" else r["memory"]["samples"][-1]["heap"])["requested_live_bytes"] / 1024**2 for r in subset)}
        if args.workload == "fixed":
            medians[capacity].update({key: statistics.median(r["report"][key] for r in subset)
                                     for key in ("elapsed_ms", "delivery_ms")})
    summary = {"workload": args.workload, "cases": results, "medians": medians,
               "test_binary_sha256": hashlib.sha256(args.test_binary.read_bytes()).hexdigest(),
               "recipe_sha256": hashlib.sha256(args.recipe.read_bytes()).hexdigest(),
               "qualification": "requested live heap excludes allocator slack, stacks and diagnostic headers; test tracer affects timing; fixed heap excludes receipt serialization; concurrent live trajectories differ"}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
