"""Export inspectable Pine host evidence without copying large traces or secrets."""
import argparse
import hashlib
import json
from pathlib import Path
import re


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.artifacts.resolve()
    names = ["screen-summary.json", "v6-candidate-abba-summary.json",
             "v6-first-block-medians.json", "v6-second-block-medians.json",
             "typed-state-direct-micro.json", "fixed-cache-abba-v3.json",
             "full-turn-abba.json", "full-turn-per-bot-identity-abba.json",
             "async-full-checkpoint-audit.json", "defender-preference-check.json",
             "prototype-patch-check.json", "selected-v11-build-result.json",
             "selected-v11-http-recovery.json", "selected-code-invariant-audit.json",
             "selected-v11-native-summary.json", "selected-v11-500-summary.json",
             "selected-v11-1000-summary.json", "selected-v11-long-summary.json",
             "completion-audit.json", "owned-postgres-closeout.json"]
    evidence = []
    for name in names:
        path = root / name
        if path.is_file():
            evidence.append({"artifact": name, "sha256": digest(path),
                             "data": json.loads(path.read_text(encoding="utf-8-sig"))})
    tests = []
    for name in ["selected-v10-workspace-postgres-tests-v2.log", "selected-v11-pine-tests.log",
                 "selected-v11-clippy-all.log", "selected-v11-clippy-default.log"]:
        path = root / name
        if path.is_file():
            body = path.read_text(encoding="utf-8-sig")
            rows = re.findall(r"test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored", body)
            tests.append({"artifact": name, "sha256": digest(path),
                          "passed": sum(int(row[0]) for row in rows),
                          "failed": sum(int(row[1]) for row in rows),
                          "ignored": sum(int(row[2]) for row in rows),
                          "finished_build_marker": "Finished `release`" in body,
                          "postgres_tests_present": [line.strip() for line in body.splitlines()
                              if "postgres" in line.lower() and " ... ok" in line]})
    qualifications = []
    qualification_root = root / "selected-v11-qualification-v3"
    if qualification_root.is_dir():
        results_path = qualification_root / "results.json"
        if results_path.is_file():
            results = json.loads(results_path.read_text())
            for row in results["cases"]:
                path = Path(row["report"])
                if digest(path) != row["report_sha256"]:
                    raise RuntimeError(f"qualification report changed: {path.name}")
                report = json.loads(path.read_text())
                stages = report.get("cases") if isinstance(report.get("cases"), list) else [report]
                compact = []
                for item in stages:
                    source = Path(item["report"]) if isinstance(item.get("report"), str) else path
                    if source != path:
                        if digest(source) != item["report_sha256"]:
                            raise RuntimeError("nested report changed")
                        data = json.loads(source.read_text())
                    else:
                        data = item
                    compact.append({"case": item.get("case", row["label"]),
                        "report_sha256": digest(source), "recipe_sha256": data.get("recipe_sha256"),
                        "qualified": data.get("qualified"), "steps_per_second": data.get("steps_per_wall_second"),
                        "cpu_cores": data.get("average_logical_cores"), "peak_memory_mib": data.get("peak_memory_mib"),
                        "latency": data.get("latency"), "duration": data.get("duration"),
                        "bot_durable_receipts": data.get("bot_durable_receipts"),
                        "diagnostics": data.get("diagnostics"), "errors": data.get("errors")})
                qualifications.append({"label": row["label"], "exit_code": row["exit_code"],
                    "sampling_valid": row.get("sampling_valid"), "report_sha256": digest(path), "cases": compact})
    exclusions = []
    for name in ["wave-abba/invalid-grouping.json", "v5-option-screen-interruption.json",
                 "selected-v11-qualification-invalid-monitor.json",
                 "selected-v11-qualification-v2-invalid-monitor.json"]:
        path = root / name
        if path.is_file():
            exclusions.append({"artifact": name, "sha256": digest(path), "data": json.loads(path.read_text())})
    result = {"schema": "marketforge.pine-host-performance/1",
              "baseline_commit": "05ce347478f9b870996e74ac66c9afc7a980b27e",
              "selected": "Source admission shared by exact immutable realtime wave and Host; public calls remain fresh.",
              "rejected_production_paths": ["identity/seal caches", "input length reuse", "chain cursor",
                  "worker/batch tuning", "wake offsets", "background JSON fragments", "checkpoint byte/time policy"],
              "prototype_boundaries": ["Typed serialization experiment is not a typed broker rewrite.",
                  "Covering-revision checkpoint audit is not a deployed independent-anchor protocol."],
              "evidence": evidence, "tests": tests, "qualification": qualifications, "exclusions": exclusions,
              "qualification_complete": (qualification_root / "complete.json").is_file(),
              "full_capacity_qualified": bool(qualifications) and all(
                  not any(value is False for value in (case.get("qualified") or {}).values())
                  for stage in qualifications for case in stage["cases"]),
              "scope": "Run-level quantiles and explicitly bounded experiments. Green execution is not capacity qualification; inspect every gate. Large raw local receipts remain outside Git."}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"evidence": len(evidence), "qualification_stages": len(qualifications),
                      "qualification_complete": result["qualification_complete"]}))


if __name__ == "__main__":
    main()
