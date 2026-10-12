"""Compare frozen withdrawal/liquidation core binaries with serial ABBA rounds."""
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
    binaries = {version: path.resolve(strict=True) for version, path in
                (("before", args.before), ("after", args.after))}
    manifest = {
        "started_utc": datetime.now(timezone.utc).isoformat(),
        "baseline_commit": args.baseline_commit,
        "binaries": {version: {"path": str(path), "sha256": sha256(path)}
                     for version, path in binaries.items()},
        "driver_sha256": sha256(Path(__file__).resolve().parents[1] /
                                "exchange-core/examples/benchmark_transaction_entries.rs"),
        "host": {"platform": platform.platform(), "logical_cpus": os.cpu_count()},
        "sequence": ["before", "after", "after", "before"] * args.rounds,
    }
    (args.output / "manifest.json").write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    keys = {(accounts, markets, workload) for accounts in (3, 100, 1000, 10000)
            for markets in (1, 4) for workload in
            ("withdrawal_accepted", "withdrawal_rejected", "liquidation")}
    hashes = {}
    samples = {version: {key: [] for key in keys} for version in binaries}
    for index, version in enumerate(manifest["sequence"]):
        completed = subprocess.run([str(binaries[version])], capture_output=True, check=False)
        prefix = args.output / f"{index:02d}-{version}"
        prefix.with_suffix(".json").write_bytes(completed.stdout)
        prefix.with_suffix(".stderr.log").write_bytes(completed.stderr)
        completed.check_returncode()
        counts = {key: 0 for key in keys}
        for row in json.loads(completed.stdout)["runs"]:
            key = row["accounts"], row["markets"], row["workload"]
            assert key in keys, f"unknown case: {key}"
            counts[key] += 1
            assert row["samples"] == 30
            assert hashes.setdefault(key, row["state_receipt_sha256"]) == row["state_receipt_sha256"], f"state/receipt mismatch: {version} {key}"
            samples[version][key].append(row)
        assert all(count == 2 for count in counts.values()), f"incomplete/repeated case matrix: {counts}"
        print(f"{index + 1}/{len(manifest['sequence'])} {version}: 24 exact state/receipt hashes verified", flush=True)
    comparisons = []
    for key in sorted(keys):
        timing = {}
        for version in binaries:
            rows = samples[version][key]
            timing[version] = {metric: {"median": statistics.median(r[metric] for r in rows),
                                       "min": min(r[metric] for r in rows),
                                       "max": max(r[metric] for r in rows)}
                               for metric in ("median_us", "p95_us")}
            timing[version]["run_samples"] = len(rows)
        timing["speedup"] = timing["before"]["median_us"]["median"] / timing["after"]["median_us"]["median"]
        comparisons.append(dict(zip(("accounts", "markets", "workload"), key),
                                timings=timing, state_receipt_sha256=hashes[key]))
    summary = {"manifest": manifest, "exact_state_and_receipts_equal": True,
               "comparisons": comparisons,
               "limits": ["Fixed core public API work, not HTTP/PG/bot capacity or long-run qualification.",
                          "3 warmups + 30 measured independent fixture clones per case, forward/reverse population order.",
                          "External fixture clones, assertions and final full actor/receipt hashing excluded from timings.",
                          "Statistics are medians of per-case medians/P95s, not pooled latency percentiles.",
                          "Accepted withdrawal means non-rejected submission, not delayed completion latency."]}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    for row in comparisons:
        if row["accounts"] == 10000:
            t = row["timings"]
            print(f"{row['markets']} markets {row['workload']}: {t['before']['median_us']['median']:.2f} -> {t['after']['median_us']['median']:.2f} us ({t['speedup']:.2f}x)")


if __name__ == "__main__":
    main()
