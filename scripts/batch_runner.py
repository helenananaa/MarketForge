#!/usr/bin/env python3
"""Headless batch runner: isolated rooms per seed, resume-by-run, keep failures."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))

from marketforge import Client, MarketForgeError  # noqa: E402


def load_state(path: Path | None) -> dict:
    if path is None or not path.exists():
        return {}
    return json.loads(path.read_text())


def save_state(path: Path | None, state: dict) -> None:
    if path is None:
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2))


def main() -> int:
    parser = argparse.ArgumentParser(description="Run isolated training seeds")
    parser.add_argument("base_url")
    parser.add_argument("spec")
    parser.add_argument("seeds", nargs="*", type=int)
    parser.add_argument("--state", help="JSON file used to resume completed runs")
    parser.add_argument("--max-steps", type=int, default=None)
    parser.add_argument("--fail-seeds", default="", help="comma-separated seeds forced to fail")
    args = parser.parse_args()

    spec = json.loads(Path(args.spec).read_text())
    seeds = args.seeds or [1]
    fail_seeds = {int(item) for item in args.fail_seeds.split(",") if item.strip()}
    state_path = Path(args.state) if args.state else None
    state = load_state(state_path)
    completed = {row["run_id"]: row for row in state.get("runs", [])}
    client = Client(args.base_url, trusted_owner_urls=[args.base_url.rstrip("/")])
    rows = []
    for seed in seeds:
        request = json.loads(json.dumps(spec))
        request["run_id"] = f"{spec.get('run_id', 'batch')}-{seed}"
        request["scenario"]["room_id"] = f"{spec['scenario']['room_id']}-{seed}"
        if args.max_steps is not None:
            request["horizon_steps"] = args.max_steps
        if seed in fail_seeds:
            request["target_qty"] = 0
        if request["run_id"] in completed:
            rows.append({**completed[request["run_id"]], "resumed": True})
            continue
        try:
            result = client.start_training(request)
            row = {
                "seed": seed,
                "ok": True,
                "run_id": request["run_id"],
                "status": result["run"]["status"],
                "score": result.get("score"),
                "versions": {
                    "spec": result["run"]["spec"]["spec_version"],
                    "task": result["run"]["spec"]["task_version"],
                    "scoring": result["run"]["spec"]["scoring_version"],
                },
            }
        except MarketForgeError as exc:
            row = {
                "seed": seed,
                "ok": False,
                "run_id": request["run_id"],
                "error": str(exc),
            }
        completed[request["run_id"]] = row
        rows.append(row)
        save_state(state_path, {"runs": list(completed.values())})
    failures = [row for row in rows if not row.get("ok")]
    print(json.dumps({"runs": rows, "failures": failures}, indent=2))
    return 0 if not failures else 1


if __name__ == "__main__":
    raise SystemExit(main())
