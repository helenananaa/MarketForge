"""Run frozen research variants sequentially and compare exact receipt bytes.

Plan JSON contains variants {name: {exe, cwd}}, cases {name: {recipe, steps,
seconds, env}}, and runs [{variant, case}]. Paths are absolute. No simulator
parameters are changed by this harness. Per-run evidence is retained on error.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
from pine_receipt_projection import receipt_projection


def digest(path):
    with open(path, "rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("plan", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    plan = json.loads(args.plan.read_text(encoding="utf-8-sig"))
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "plan.json").write_text(json.dumps(plan, indent=2), encoding="utf-8")
    summary = {"plan_sha256": digest(args.plan), "runs": [], "all_equal": True}
    expected = dict(plan.get("expected_receipt_sha256", {}))
    projected = bool(plan.get("pine_storage_projection", False))
    if projected:
        expected = dict(plan["expected_projected_receipt_sha256"])
    for number, run in enumerate(plan["runs"]):
        variant, case = plan["variants"][run["variant"]], plan["cases"][run["case"]]
        stem = f'{number:02}-{run["variant"]}-{run["case"]}'
        receipt, log = args.output / (stem + ".receipt.json"), args.output / (stem + ".log")
        env = {k: v for k, v in os.environ.items()
               if not k.startswith(("RESEARCH_", "MF_RESEARCH_", "MARKETFORGE_BOT_", "MARKETFORGE_PINE_", "PINE_"))}
        env.update({"RESEARCH_PROFILE": "1", **case.get("env", {}), **variant.get("env", {})})
        command = [variant["exe"], case["recipe"], str(case["steps"]), str(case["seconds"])]
        print(f"start {stem}", flush=True)
        started = time.perf_counter()
        resources = None
        with receipt.open("wb") as stdout, log.open("wb") as stderr:
            if plan.get("sample_resources", False):
                from process_tree_metrics import ProcessTreeMetrics
                with subprocess.Popen(command, cwd=variant["cwd"], env=env, stdout=stdout, stderr=stderr) as result:
                    sampler = ProcessTreeMetrics(result.pid)
                    try:
                        result.wait(timeout=plan.get("timeout", 1200))
                    except subprocess.TimeoutExpired:
                        subprocess.run(["taskkill", "/PID", str(result.pid), "/T", "/F"], capture_output=True)
                        raise
                    finally:
                        resources = sampler.finish()
            else:
                result = subprocess.run(command, cwd=variant["cwd"], env=env,
                                        stdout=stdout, stderr=stderr, timeout=plan.get("timeout", 1200))
        wall = time.perf_counter() - started
        profiles = [line for line in log.read_text(encoding="utf-8").splitlines() if line.startswith("profile ")]
        last = profiles[-1] if profiles else ""
        phases = {key: float(value) for key, value in re.findall(r"(\w+)=(\d+(?:\.\d+)?)s", last)}
        sha = digest(receipt)
        compared_sha = sha
        if projected and result.returncode == 0:
            canonical = json.dumps(receipt_projection(json.loads(receipt.read_bytes())),
                                   sort_keys=True, separators=(",", ":")).encode()
            compared_sha = hashlib.sha256(canonical).hexdigest()
        expected.setdefault(run["case"], compared_sha)
        equal = compared_sha == expected[run["case"]]
        item = {**run, "wall_seconds": wall, "phases_seconds": phases,
                "receipt_sha256": sha, "equal": equal, "exit_code": result.returncode,
                "exe_sha256": digest(variant["exe"]), "recipe_sha256": digest(case["recipe"]),
                "profile": last, "receipt": str(receipt.resolve()), "log": str(log.resolve())}
        item["runtime_counters"] = [line for line in log.read_text(encoding="utf-8").splitlines()
                                    if line.startswith("marketforge_bot_runtime_")]
        item["comparison"] = "economic_receipts_and_broker_state" if projected else "exact_bytes"
        item["compared_sha256"] = compared_sha
        if resources is not None:
            resource_path = args.output / (stem + ".resources.json")
            resource_path.write_text(json.dumps(resources, indent=2), encoding="utf-8")
            item["resources"] = {k: resources[k] for k in ("peak_sampled_rss_mib", "peak_processes",
                "average_logical_cores", "compiler_nonempty_samples", "errors")}
        if result.returncode == 0:
            payload = json.loads(receipt.read_bytes())
            item["completed"] = (payload["steps"] == case["steps"] and
                                 payload["clock_step_seconds"] == case["seconds"])
            item["trades"] = payload["trades"]
        else:
            item["completed"] = False
        summary["runs"].append(item)
        summary["all_equal"] &= equal and item["completed"]
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
        print(json.dumps({k: item[k] for k in ("variant", "case", "wall_seconds", "phases_seconds", "equal", "completed")}), flush=True)
        if not equal or not item["completed"]:
            raise SystemExit(f"failed {stem}; evidence retained at {args.output}")


if __name__ == "__main__":
    main()
