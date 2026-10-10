"""Compare unchanged public-API workloads in two isolated Release binaries.

All serialization is outside the operation timer. Every run must have byte-identical
canonical state and ordered execution receipts before timings are accepted.
"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


CASES = {
    "sparse-1k": (1000, 20, 2, False, "price", 400),
    "sparse-10k": (10000, 20, 2, False, "price", 200),
    "dense-1k": (1000, 1000, 2, False, "price", 200),
    "sparse-8markets": (1000, 20, 8, False, "price", 200),
    "dense-8markets": (1000, 1000, 8, False, "price", 120),
    "hedge-sparse": (1000, 20, 2, True, "price", 200),
    "same-price": (1000, 20, 2, False, "same-price", 400),
    "orders": (1000, 20, 2, False, "orders", 600),
    "clock": (1000, 20, 2, False, "clock", 400),
    "liquidation": (1000, 20, 2, False, "liquidation", 20),
    "threshold-sparse": (1000, 248, 2, False, "price", 200),
    "threshold-dense": (1000, 250, 2, False, "price", 200),
}


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", type=Path, required=True)
    parser.add_argument("--after", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cases", nargs="+", choices=CASES, default=list(CASES))
    parser.add_argument("--blocks", type=int, default=2)
    args = parser.parse_args()
    if args.blocks < 1:
        parser.error("blocks must be positive")
    args.output.mkdir(parents=True, exist_ok=True)
    binaries = {"before": args.before.resolve(), "after": args.after.resolve()}
    report = {"binaries": {k: {"path": str(v), "sha256": sha(v)} for k, v in binaries.items()},
              "definition": "bounded fixed public-API work; restored position fixtures; warmup and serialization excluded; no service-capacity claim",
              "cases": {}}
    for name in args.cases:
        fixture = CASES[name]
        results = {key: [] for key in binaries}
        expected = None
        for index, variant in enumerate(["before", "after", "after", "before"] * args.blocks):
            output = args.output / name / f"{index:02d}-{variant}"
            output.mkdir(parents=True, exist_ok=False)
            command = [str(binaries[variant]), *[str(v).lower() for v in fixture], str(output.resolve())]
            completed = subprocess.run(command, text=True, capture_output=True, timeout=120, check=False)
            (output / "process.log").write_text(completed.stdout + completed.stderr, encoding="utf-8")
            if completed.returncode:
                raise RuntimeError(f"{name} {variant} failed; see {output}")
            hashes = {key: sha(output / key) for key in ["state.json", "executions.txt"]}
            if expected is None:
                expected = hashes
            if hashes != expected:
                raise RuntimeError(f"{name} {variant} differs from predecessor: {hashes} != {expected}; receipts preserved at {output}")
            metrics = json.loads((output / "metrics.json").read_text())
            results[variant].append(metrics)
        before = statistics.median(row["elapsed_ms"] for row in results["before"])
        after = statistics.median(row["elapsed_ms"] for row in results["after"])
        summary = {"fixture": fixture, "equivalent_sha256": expected, "samples": results,
                   "before_median_ms": before, "after_median_ms": after,
                   "time_reduction_percent": 100 * (1 - after / before), "speedup": before / after}
        report["cases"][name] = summary
        (args.output / "comparison.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(f"{name}: {before:.3f} -> {after:.3f} ms; reduction {summary['time_reduction_percent']:.2f}%; {summary['speedup']:.2f}x; all state/receipts equal", flush=True)


if __name__ == "__main__":
    main()
