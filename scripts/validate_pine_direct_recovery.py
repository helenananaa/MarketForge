"""Restart the real research gateway across Python/Rust handoffs or Rust replay state.

Every segment is a new OS process. Only the checkpoint filename is normalized
when comparing schema-2 receipts. Replay mode excludes enumerated execution
storage and observation-fingerprint fields across runtimes, retains every trading
field, and additionally requires exact Rust state after OS restart. This is file recovery, not PostgreSQL qualification.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
from pine_receipt_projection import receipt_projection


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--cwd", type=Path, required=True)
    parser.add_argument("--recipe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--segment-steps", type=int, default=100)
    parser.add_argument("--require-direct-segments", action="store_true",
                        help="require actual direct calls in every direct-enabled segment")
    parser.add_argument("--replay-state", action="store_true")
    args = parser.parse_args()
    if args.segment_steps < 1:
        parser.error("segment steps must be positive")
    size, total = args.segment_steps, 3 * args.segment_steps
    args.output.mkdir(parents=True, exist_ok=False)
    summary = {"exe_sha256": hashlib.sha256(args.exe.read_bytes()).hexdigest(),
               "recipe_sha256": hashlib.sha256(args.recipe.read_bytes()).hexdigest(),
               "segment_steps": size, "require_direct_segments": args.require_direct_segments, "runs": []}
    expected = None
    expected_audit = None
    runs = (("process", [(total, False)]), ("direct", [(total, True)]),
                           ("process-direct-process", [(size, False), (2*size, True), (total, False)]),
                           ("direct-process-direct", [(size, True), (2*size, False), (total, True)]))
    if args.replay_state:
        runs = (("process", [(total, False)]), ("direct", [(total, True)]),
                ("direct-restarted", [(size, True), (2*size, True), (total, True)]),
                ("process-direct-restarted", [(size, False), (2*size, True), (total, True)]))
    direct_state = None
    for name, segments in runs:
        checkpoint = (args.output / f"{name}.checkpoint.json").resolve()
        audit = (args.output / f"{name}.audit.jsonl").resolve()
        route_evidence = []
        for step, direct in segments:
            env = {k: v for k, v in os.environ.items() if not k.startswith(("MARKETFORGE_", "RESEARCH_", "MF_RESEARCH_", "PINE_"))}
            env.update(MARKETFORGE_PINE_IN_PROCESS="1" if direct else "0", MARKETFORGE_PINE_VM_CHECKPOINT="0",
                       MARKETFORGE_PINE_REPLAY_STATE="1" if args.replay_state else "0",
                       MF_RESEARCH_CHECKPOINT=str(checkpoint), MF_RESEARCH_BUILD_ID="pine-direct-parity-v1",
                       RESEARCH_PLUGIN_WAKES="events", RESEARCH_PROFILE="1", RESEARCH_ORDER_AUDIT_PATH=str(audit))
            output, log = args.output / f"{name}-{step}.json", args.output / f"{name}-{step}.log"
            with output.open("wb") as stdout, log.open("wb") as stderr:
                subprocess.run([str(args.exe.resolve()), str(args.recipe.resolve()), str(step), "1"],
                               cwd=args.cwd, env=env, stdout=stdout, stderr=stderr, check=True, timeout=600)
            log_text = log.read_text(encoding="utf-8")
            if step > segments[0][0] and f"resumed_step={step - size}" not in log_text:
                raise AssertionError("missing actual restart evidence")
            direct_counts = re.findall(r'marketforge_bot_runtime_route_total\{route="direct",reason="none"\} (\d+)', log_text)
            direct_calls = int(direct_counts[-1]) if direct_counts else 0
            route_evidence.append({"step":step, "requested_direct":direct, "direct_calls":direct_calls})
            if args.require_direct_segments and direct and not direct_calls:
                raise AssertionError("direct-enabled segment did not execute directly")
            if args.replay_state and direct and 'reason="ecosystem_history"' in log_text:
                raise AssertionError("history length caused a Python fallback")
        receipt = json.loads(output.read_bytes())
        if receipt["steps"] != total or not receipt["run_complete"]:
            raise AssertionError("incomplete recovery run")
        receipt.pop("checkpoint")
        if args.replay_state:
            if name == "direct":
                direct_state = receipt["states"]
            elif name == "direct-restarted" and receipt["states"] != direct_state:
                raise AssertionError("full Rust durable state differs after OS restart")
            receipt = receipt_projection(receipt)
        canonical = json.dumps(receipt, sort_keys=True, separators=(",", ":")).encode()
        if expected is None:
            expected = canonical
        with audit.open("rb") as stream:
            audit_sha = hashlib.file_digest(stream, "sha256").hexdigest()
        if expected_audit is None:
            expected_audit = audit_sha
        row = {"audit_sha256": audit_sha, "audit_equal": audit_sha == expected_audit, "name": name, "segments": segments, "route_evidence":route_evidence, "sha256": hashlib.sha256(canonical).hexdigest(),
               "equal": canonical == expected, "trades": receipt["trades"]}
        summary["runs"].append(row)
        (args.output / "summary.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
        print(json.dumps(row), flush=True)
        if canonical != expected or audit_sha != expected_audit:
            raise AssertionError("complete recovery receipts differ")


if __name__ == "__main__":
    main()
