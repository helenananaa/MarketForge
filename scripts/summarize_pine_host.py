"""Audit retained host experiments and export compact, source-bound evidence."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics

VALIDITY = ("compiler_quiet", "no_parallel_benchmark_server", "no_harness_errors",
            "full_requested_duration_observed", "metric_window_captured", "trace_shutdown_complete")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--series", type=Path, nargs="+", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    summaries = []
    for directory in args.series:
        result_path = directory / "results.json"
        result = json.loads(result_path.read_text(encoding="utf-8"))
        groups = {}
        audited = []
        for row in result["cases"]:
            path = Path(row["report"])
            actual = hashlib.sha256(path.read_bytes()).hexdigest()
            if actual != row["report_sha256"]:
                raise RuntimeError(f"report changed: {path}")
            report = json.loads(path.read_text(encoding="utf-8"))
            qualified = row["qualified"]
            valid = row["exit_code"] == 0 and all(qualified.get(key) is True for key in VALIDITY)
            item = {"case":row["case"], "valid":valid,"report_sha256":actual,
                    "qualified":qualified,"actual_server_experiments":row.get("actual_server_experiments"),
                    "accepted":row["accepted"],"bar_to_decision":row["bar_to_decision"],
                    "cpu_cores":row["logical_cores"],"peak_memory_mib":row["peak_memory_mib"],
                    "turns":row["turns"],"phase_per_turn_ms":row["phase_per_turn_ms"],
                    "phase_counts":row["phase_counts"],
                    "prewarm_sha256":hashlib.sha256((path.parent/"prewarm-candles.json").read_bytes()).hexdigest(),
                    "recipe_sha256":report["recipe_sha256"]}
            audited.append(item)
            groups.setdefault(item["case"], []).append(item)
        medians = {case: {
            "runs":len(rows), "all_valid":all(row["valid"] for row in rows),
            "accepted_p95_ms":statistics.median(row["accepted"]["p95_ms"] for row in rows),
            "bar_to_decision_p95_ms":statistics.median(row["bar_to_decision"]["p95_ms"] for row in rows),
            "turn_ms":statistics.median(row["phase_per_turn_ms"]["bot_turn"] for row in rows),
            "cpu_cores":statistics.median(row["cpu_cores"] for row in rows),
            "peak_memory_mib":statistics.median(row["peak_memory_mib"] for row in rows),
        } for case, rows in groups.items()}
        summaries.append({"series":str(directory),"result_sha256":hashlib.sha256(result_path.read_bytes()).hexdigest(),
            "freeze":result["freeze"],"same_recipe":len({row["recipe_sha256"] for row in audited})==1,
            "same_prewarm":len({row["prewarm_sha256"] for row in audited})==1,
            "groups":medians,"cases":audited,
            "scope":"Medians of run-level quantiles; differing asynchronous action populations. Single-run groups are exploratory; nested elapsed scopes are not additive or CPU shares."})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"series":summaries},indent=2,ensure_ascii=False)+"\n",encoding="utf-8")
    print(json.dumps([{ "series":row["series"],"same_recipe":row["same_recipe"],
                       "same_prewarm":row["same_prewarm"],"groups":row["groups"]} for row in summaries],indent=2))


if __name__ == "__main__":
    main()
