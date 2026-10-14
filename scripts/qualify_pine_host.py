"""Serial final-code native controls, Pine ABBA tiers, and sustained validation."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

from benchmark_pine_dispatch_checkpoint import frozen_inputs, changed_inputs
from process_tree_metrics import ProcessTreeMetrics

ROOT = Path(__file__).resolve().parents[1]


def save(path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--selected", type=Path, required=True)
    parser.add_argument("--plugin-dir", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.output.exists():
        parser.error("preserve prior outcomes; output must be new")
    args.output.mkdir(parents=True)
    binaries = {"baseline": args.baseline.resolve(), "selected": args.selected.resolve()}
    plugins = {name: args.plugin_dir.resolve() for name in binaries}
    freeze = frozen_inputs(binaries, plugins)
    controller_paths = [Path(__file__), ROOT / "scripts/benchmark_pine_host.py"]
    controller_hashes = {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for path in controller_paths}
    save(args.output / "freeze.json", {**freeze, "controllers": controller_hashes})
    rows = []

    def run(label, command, report):
        if changed_inputs(freeze, binaries, plugins) or any(
            hashlib.sha256(Path(path).read_bytes()).hexdigest() != value
            for path, value in controller_hashes.items()
        ):
            raise RuntimeError("frozen qualification inputs changed")
        env = os.environ.copy()
        for key in list(env):
            if key.startswith("MARKETFORGE_") or key.startswith("PINE_BENCH_"):
                env.pop(key)
        with (args.output / f"{label}.log").open("w", encoding="utf-8") as log:
            process = subprocess.Popen(command, cwd=ROOT, env=env, stdout=log, stderr=subprocess.STDOUT)
            monitor = ProcessTreeMetrics(process.pid, driver_root=True) if label.startswith("native-") else None
            try:
                process.wait()
            finally:
                tree = monitor.finish() if monitor else None
                if tree:
                    save(args.output / f"{label}-process-tree.json", tree)
        row = {"label": label, "command": command, "exit_code": process.returncode, "report": str(report)}
        if report.is_file():
            row["report_sha256"] = hashlib.sha256(report.read_bytes()).hexdigest()
        if tree:
            row["sampling_valid"] = bool(tree["samples"]) and not tree["errors"] and (
                tree["compiler_nonempty_samples"] == tree["other_server_nonempty_samples"] == 0
                and tree["max_owned_benchmark_servers"] == 1)
        rows.append(row)
        save(args.output / "results.json", {"cases": rows, "scope": "Validation receipts; capacity failures remain failures."})
        print(json.dumps(row), flush=True)
        if process.returncode or not report.is_file():
            raise RuntimeError(f"incomplete qualification stage {label}; preserve receipt")
        if tree and not row["sampling_valid"]:
            raise RuntimeError(f"contaminated native measurement {label}; preserve receipt")

    for index, version in enumerate(["baseline", "selected", "selected", "baseline"], 1):
        label = f"native-{index:02d}-{version}"
        output = args.output / label
        run(label, [sys.executable, str(ROOT / "scripts/benchmark_market_load.py"),
            "--server", str(binaries[version]), "--output", str(output), "--counts", "1000",
            "--warmup", "5", "--seconds", "60", "--interval-ms", "25", "--max-memory-mib", "4096"],
            output / "bots-1000/report.json")
    for count in [500, 1000]:
        label = f"pine-{count}-abba"
        output = args.output / label
        run(label, [sys.executable, str(ROOT / "scripts/benchmark_pine_host.py"),
            "--server", str(binaries["selected"]), "--plugin-dir", str(plugins["selected"]),
            "--output", str(output), "--counts", str(count), "--seconds", "120",
            "--cases", "control", "wave", "wave", "control"], output / "results.json")
    label = "pine-1000-35min"
    output = args.output / label
    run(label, [sys.executable, str(ROOT / "scripts/benchmark_strategy_runtime.py"),
        "--server", str(binaries["selected"]), "--plugin-dir", str(plugins["selected"]),
        "--output", str(output), "--counts", "1000", "--seconds", "2100", "--long-run",
        "--population", "pine", "--interval-ms", "100", "--warmup", "20", "--prewarm-bars", "64",
        "--sustained-input", "--input-profile", "cycle", "--pine-profile", "diverse-short",
        "--pine-in-process", "--pine-vm-checkpoint", "--require-diagnostics", "--require-bot-receipts",
        "--trace-graceful-shutdown", "--host-experiment", "MARKETFORGE_PINE_SOURCE_WAVE=1"],
        output / "bots-1000/report.json")
    if changed_inputs(freeze, binaries, plugins):
        raise RuntimeError("frozen inputs changed at closeout")
    save(args.output / "complete.json", {"stages": len(rows), "frozen_inputs_unchanged": True})


if __name__ == "__main__":
    main()
