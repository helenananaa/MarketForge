"""Reconcile isolated load receipts; keep compiler-contaminated runs separate."""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path
import platform
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", type=Path, nargs="+", required=True)
    parser.add_argument("--probe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    rows = []
    for folder in args.inputs:
        for receipt in sorted(folder.glob("*/report.json")):
            row = json.loads(receipt.read_text(encoding="utf-8"))
            row["evidence_path"] = str(receipt.resolve())
            row["eligible_comparison"] = not row["errors"] and row["compiler_overlap_samples"] == 0
            row.pop("resources", None)
            rows.append(row)
    groups = defaultdict(list)
    for row in rows:
        if row["eligible_comparison"]:
            groups[(row["kind"], row["workload"], row["concurrency"], row["topology"], row["cycles"], row["server_sha256"])].append(row)
    def bounds(items, field):
        values = [field(item) for item in items]
        return [min(values), max(values)]
    aggregated = []
    for key, items in sorted(groups.items()):
        kind, workload, concurrency, topology, cycles, server = key
        if len(items) < 2:
            continue
        def stage(name):
            values = [row["stages"][name]["mean_ms"] for row in items if name in row["stages"]]
            return [min(values), max(values)] if values else None
        aggregated.append({"kind": kind, "workload": workload, "concurrency": concurrency,
                           "topology": topology, "cycles": cycles, "server_sha256": server, "runs": len(items),
                           "commands_per_second": bounds(items, lambda r: r["accepted_commands_per_second"]),
                           "p95_ms": bounds(items, lambda r: r["http_latency"]["p95_ms"]),
                           "read_p95_ms": bounds(items, lambda r: r["read_probe_latency"]["p95_ms"]),
                           "wait_mean_ms": stage("room_wait"), "held_mean_ms": stage("room_held"),
                           "prepare_mean_ms": stage("order_prepare"), "apply_mean_ms": stage("order_apply"),
                           "journal_mean_ms": stage("order_journal"),
                           "peak_memory_mib": bounds(items, lambda r: r["peak_memory_mib"]),
                           "average_server_cores": bounds(items, lambda r: r["server_average_cores"]),
                           "evidence": [r["evidence_path"] for r in items]})
    sources = ["exchange-server/Cargo.toml", "exchange-server/src/lib.rs", "exchange-server/src/room_runtime.rs",
               "exchange-server/src/bench_execution_timing.rs", "exchange-server/examples/benchmark_room_http.rs",
               "scripts/benchmark_room_contention.py", "scripts/probe_room_head_of_line.py", "scripts/summarize_room_contention.py"]
    binaries = [root / "target/room-lock-load/baseline.exe", root / "target/room-lock-load/diagnostic.exe",
                root / "target/release/examples/benchmark_room_http.exe"]
    probe = json.loads(args.probe.read_text(encoding="utf-8"))
    result = {"base_commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=root, text=True).strip(),
              "branch": subprocess.check_output(["git", "branch", "--show-current"], cwd=root, text=True).strip(),
              "platform": platform.platform(), "python": platform.python_version(),
              "cpu": "Intel Core i9-13900HX; 24 cores, 32 logical processors",
              "sources_sha256": {p: hashlib.sha256((root / p).read_bytes()).hexdigest() for p in sources},
              "binary_sha256": {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in binaries},
              "case_count": len(rows), "eligible_case_count": sum(r["eligible_comparison"] for r in rows),
              "excluded_compiler_overlap_count": sum(r["compiler_overlap_samples"] > 0 for r in rows),
              "measured_commands": sum(r["validation"]["completed_commands"] for r in rows),
              "errors": [r["evidence_path"] for r in rows if r["errors"]],
              "groups": aggregated, "cases": rows, "head_of_line": probe,
              "limits": ["In-memory journal; no PostgreSQL capacity or crash-recovery qualification.",
                         "Closed-loop fixed-work bursts; not sustained arrival-rate or production P95 SLOs.",
                         "Account population grows with concurrency; comparisons are within a concurrency tier.",
                         "Room topology also changes snapshot size and per-room history length, not only locks.",
                         "Background applications remain running; their sampled CPU is retained in each receipt.",
                         "Timing counters are benchmark-feature-only; no production architecture change.",
                         "Clean committed main baseline excludes unrelated uncommitted optimization work."]}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({key: result[key] for key in ("case_count", "eligible_case_count", "excluded_compiler_overlap_count",
                      "measured_commands", "errors")}), flush=True)


if __name__ == "__main__":
    main()
