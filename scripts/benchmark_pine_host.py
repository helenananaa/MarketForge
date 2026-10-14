"""Serial same-binary Pine host experiments with frozen inputs and full receipts.

Experimental flags must be explicit driver arguments: the lower-level load
driver intentionally strips inherited MARKETFORGE_* environment variables.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

from benchmark_pine_dispatch_checkpoint import frozen_inputs, changed_inputs

ROOT = Path(__file__).resolve().parents[1]
FLAGS = ("MARKETFORGE_PINE_SOURCE_WAVE", "MARKETFORGE_PINE_IDENTITY_CACHE",
         "MARKETFORGE_PINE_SEAL_CACHE", "MARKETFORGE_PINE_INPUT_LENGTH",
         "MARKETFORGE_PINE_CHAIN_FINGERPRINT",
         "MARKETFORGE_EXTERNAL_DECISION_WORKERS", "MARKETFORGE_PINE_TASK_BATCH",
         "MARKETFORGE_PINE_BACKGROUND_JSON", "MARKETFORGE_PINE_WAKE_SPREAD_MS",
         "MARKETFORGE_SCHEDULER_CHECKPOINT_BYTES", "MARKETFORGE_SCHEDULER_CHECKPOINT_MS")
CASES = {
    "control": {FLAGS[0]: "0"},
    "wave": {FLAGS[0]: "1"},
}


def save(path, value):
    path.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def summarize(report):
    receipt = report.get("bot_durable_receipts") or {}
    phases = (report.get("diagnostics") or {}).get("measurement_deltas") or {}
    turns = phases.get("bot_turn", {}).get("count", 0)
    return {
        "recipe_sha256": report.get("recipe_sha256"),
        "qualified": report.get("qualified"),
        "errors": report.get("errors"),
        "steps_per_second": report.get("steps_per_wall_second"),
        "logical_cores": report.get("average_logical_cores"),
        "peak_memory_mib": report.get("peak_memory_mib"),
        "accepted": receipt.get("accepted_bar_close_to_durable"),
        "bar_to_decision": receipt.get("bar_close_to_decision"),
        "accepted_coverage": receipt.get("accepted_window_coverage", {}).get("qualified"),
        "turns": turns,
        "phase_per_turn_ms": {key: value["total_wall_ms"] / turns if turns else None
                              for key, value in phases.items()},
        "phase_counts": {key: value["count"] for key, value in phases.items()},
        "scope": "Nested elapsed scopes are not additive or CPU shares; accepted mixes may differ.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--plugin-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=["control", "wave", "wave", "control"])
    parser.add_argument("--counts", type=int, default=1000)
    parser.add_argument("--seconds", type=int, default=120)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("output must be new; preserve earlier attempts")
    args.output.mkdir(parents=True)
    binary = args.server.resolve()
    plugins = args.plugin_dir.resolve()
    binaries, directories = {"experiment": binary}, {"experiment": plugins}
    freeze = frozen_inputs(binaries, directories)
    controller = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
    save(args.output / "freeze.json", {**freeze, "controller_sha256": controller,
                                     "cases": args.cases, "count": args.counts, "seconds": args.seconds})
    rows = []
    for index, case in enumerate(args.cases, 1):
        changes = changed_inputs(freeze, binaries, directories)
        if changes or hashlib.sha256(Path(__file__).read_bytes()).hexdigest() != controller:
            raise RuntimeError(f"frozen inputs changed: {changes}")
        output = args.output / f"{index:02d}-{case}"
        env = os.environ.copy()
        for flag in FLAGS:
            env.pop(flag, None)
        env.update(CASES[case])
        command = [sys.executable, str(ROOT / "scripts/benchmark_strategy_runtime.py"),
                   "--server", str(binary), "--plugin-dir", str(plugins), "--output", str(output),
                   "--counts", str(args.counts), "--seconds", str(args.seconds),
                   "--population", "pine", "--interval-ms", "100", "--warmup", "20",
                   "--prewarm-bars", "64", "--sustained-input", "--input-profile", "cycle",
                   "--pine-profile", "diverse-short", "--pine-in-process", "--pine-vm-checkpoint",
                   "--require-diagnostics", "--require-bot-receipts", "--trace-graceful-shutdown"]
        for name, value in CASES[case].items():
            command += ["--host-experiment", f"{name}={value}"]
        with (args.output / f"{index:02d}-{case}.log").open("w", encoding="utf-8") as log:
            result = subprocess.run(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
        report = output / f"bots-{args.counts}/report.json"
        row = {"case": case, "env": CASES[case], "exit_code": result.returncode,
               "command": command, "report": str(report)}
        if report.is_file():
            row.update(summarize(json.loads(report.read_text(encoding="utf-8"))))
            row["report_sha256"] = hashlib.sha256(report.read_bytes()).hexdigest()
            row["prewarm_candles_sha256"] = hashlib.sha256((report.parent / "prewarm-candles.json").read_bytes()).hexdigest()
            actual = json.loads((report.parent / "host-experiment-env.json").read_text())
            row["actual_server_experiments"] = actual
            if any(actual.get(name) != value for name, value in CASES[case].items()):
                raise RuntimeError("experiment options did not reach the server")
        rows.append(row)
        save(args.output / "results.json", {"freeze": freeze, "cases": rows,
                                            "scope": "Short memory-journal comparisons; not long-run or PostgreSQL capacity."})
        print(json.dumps({key: row.get(key) for key in ("case", "exit_code", "accepted", "bar_to_decision", "logical_cores")}), flush=True)
        if result.returncode or not report.is_file():
            raise RuntimeError(f"case {case} did not produce a completed run; receipt preserved")
        validity = row["qualified"]
        if not all(validity.get(key) is True for key in (
            "compiler_quiet", "no_parallel_benchmark_server", "no_harness_errors",
            "full_requested_duration_observed", "metric_window_captured", "trace_shutdown_complete",
        )):
            raise RuntimeError(f"case {case} has invalid measurement; receipt preserved")
    if changed_inputs(freeze, binaries, directories):
        raise RuntimeError("frozen inputs changed after final case")
    if len({row["recipe_sha256"] for row in rows}) != 1:
        raise RuntimeError("recipes differ")
    if len({row["prewarm_candles_sha256"] for row in rows}) != 1:
        raise RuntimeError("initial histories differ")


if __name__ == "__main__":
    main()
