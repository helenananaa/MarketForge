"""Compare two frozen core binaries using serial ABBA rounds and exact receipts."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import statistics
import subprocess
from datetime import datetime, timezone
from pathlib import Path


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--baseline-commit", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--rounds", type=int, default=2)
    args = parser.parse_args()
    if args.rounds < 1:
        parser.error("rounds must be positive")
    args.output.mkdir(parents=True, exist_ok=False)
    binaries = {"before": args.before.resolve(strict=True), "after": args.after.resolve(strict=True)}
    manifest = {
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "baseline_commit": args.baseline_commit,
        "binaries": {version: {"path": str(path), "sha256": sha256(path)} for version, path in binaries.items()},
        "driver_sha256": sha256(Path(__file__).resolve().parents[1] / "exchange-core/examples/benchmark_account_scope.rs"),
        "host": {"platform": platform.platform(), "logical_cpus": os.cpu_count()},
        "sequence": [version for _ in range(args.rounds) for version in ("before", "after", "after", "before")],
    }
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    reports = {"before": [], "after": []}
    receipts = {}
    expected_keys = None
    for index, version in enumerate(manifest["sequence"]):
        completed = subprocess.run(
            [str(binaries[version])], capture_output=True, timeout=300,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
        )
        prefix = args.output / f"{index:02d}-{version}"
        prefix.with_suffix(".json").write_bytes(completed.stdout)
        prefix.with_suffix(".stderr.log").write_bytes(completed.stderr)
        completed.check_returncode()
        report = json.loads(completed.stdout)
        keys = set()
        for row in report["runs"]:
            key = row["accounts"], row["kind"], row["workload"]
            keys.add(key)
            assert row["commands"] == 400
            assert row["first"]["samples"] == row["second"]["samples"] == 180
            assert row["trades"] == (200 if row["workload"] == "maker_taker" else 0)
            assert receipts.setdefault(key, row["receipt_sha256"]) == row["receipt_sha256"], f"state or receipt differs: {version} {key}"
            reports[version].append(row)
        if expected_keys is None:
            expected_keys = keys
            assert len(keys) == 14, f"incomplete workload matrix: {keys}"
        assert keys == expected_keys, "missing or extra workload"
        print(f"{index + 1}/{len(manifest['sequence'])} {version}: exact state/receipt hashes verified", flush=True)
    summary = []
    for key in sorted(expected_keys):
        row = {"accounts": key[0], "kind": key[1], "workload": key[2], "receipt_sha256": receipts[key]}
        for command in ("first", "second"):
            phases = {}
            for version, samples in reports.items():
                selected = [sample[command] for sample in samples if (sample["accounts"], sample["kind"], sample["workload"]) == key]
                phases[version] = {
                    metric: {"median": statistics.median([s[metric] for s in selected]),
                             "min": min(s[metric] for s in selected), "max": max(s[metric] for s in selected)}
                    for metric in ("median_us", "p95_us")
                }
                phases[version]["round_samples"] = len(selected)
            phases["speedup"] = phases["before"]["median_us"]["median"] / phases["after"]["median_us"]["median"]
            row[command] = phases
        summary.append(row)
    output = {"manifest": manifest, "exact_state_and_receipts_equal": True, "comparisons": summary,
              "limits": ["Fixed core work, not HTTP/PG/bot capacity or long-run qualification.",
                         "Statistics are medians of per-case run medians/P95s, not pooled latency percentiles.",
                         "Timings exclude setup, assertions, final state/receipt serialization and hashing.",
                         "Growing history and every command/trade count are identical before and after."]}
    (args.output / "summary.json").write_text(json.dumps(output, indent=2), encoding="utf-8")
    for row in summary:
        if row["accounts"] == 10000:
            for phase in ("first", "second"):
                result = row[phase]
                print(f"{row['kind']} {row['workload']} {phase}: {result['before']['median_us']['median']:.2f} -> {result['after']['median_us']['median']:.2f} us ({result['speedup']:.2f}x)")


if __name__ == "__main__":
    main()
