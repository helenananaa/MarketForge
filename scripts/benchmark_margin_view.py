"""Paired fixed-state view construction/JSON serialization, without network."""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["before", "after", "state", "output"]:
        parser.add_argument("--"+name, type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=5000)
    parser.add_argument("--blocks", type=int, default=3)
    parser.add_argument("--instrument", default="m0")
    args = parser.parse_args()
    assert args.iterations > 0 and args.blocks > 0
    bins = {k: getattr(args, k).resolve() for k in ["before", "after"]}
    args.state = args.state.resolve()
    args.output.mkdir(parents=True, exist_ok=False)
    result = {"definition": __doc__, "state_sha256": hashlib.sha256(args.state.read_bytes()).hexdigest(),
        "instrument": args.instrument, "iterations": args.iterations, "runs": [],
        "binaries": {k: {"path": str(v), "sha256": hashlib.sha256(v.read_bytes()).hexdigest()} for k, v in bins.items()}}
    expected = None
    for index, variant in enumerate(["before", "after", "after", "before"] * args.blocks):
        folder = args.output/f"{index:02d}-{variant}"
        receipt = subprocess.run([str(bins[variant]), str(args.state), args.instrument, str(args.iterations), str(folder.resolve())],
            capture_output=True, text=True, check=True)
        point = json.loads((folder/"result.json").read_text())
        checksum = hashlib.sha256((folder/"view.json").read_bytes()).hexdigest()
        if expected is None: expected = checksum
        assert checksum == expected, "view response differs; receipts preserved"
        result["runs"].append({"index": index, "variant": variant, "view_sha256": checksum, **point})
        print(variant, receipt.stdout.strip(), flush=True)
        (args.output/"comparison.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    result["medians"] = {v: {k: statistics.median(r[k] for r in result["runs"] if r["variant"] == v)
        for k in ["construct_ms", "serialize_ms"]} for v in bins}
    (args.output/"comparison.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result["medians"], indent=2), flush=True)


if __name__ == "__main__":
    main()
