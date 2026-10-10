"""Run real matching against the fitted recipe and expose reference residuals."""
import argparse
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from marketforge.calibration import ReferenceTrade, Segment, paired_summary


def aggregate_receipts(rows, *, require_order_ids=False):
    """Conservative contiguous aggregation preserving the actual price path.

    Spot merges one taker order at one price/time. Perpetual merges adjacent
    equal-price/side prints in a 100ms bucket. Simulation clocks are 1s, so this
    is a reported proxy, not a reproduction of exchange intrasecond timing.
    Legacy receipts without order IDs stay raw rather than inventing ownership.
    """
    grouped = []
    last_by_instrument = {}
    for index, row in enumerate(rows):
        if "taker_order_id" not in row:
            if require_order_ids:
                raise ValueError("joint calibration requires taker order IDs")
            key = ("raw", index)
        elif row["instrument_id"].endswith("SPOT"):
            key = (row["time_ms"], row["price_tick"], row["taker_side"], row["taker_order_id"])
        else:
            key = (row["time_ms"] // 100, row["price_tick"], row["taker_side"])
        last = last_by_instrument.get(row["instrument_id"])
        if last is not None and last[0] == key:
            last[1]["qty"] += row["qty"]
            last[1]["underlying_trades"] += 1
        else:
            merged = dict(row, underlying_trades=1)
            grouped.append(merged)
            last_by_instrument[row["instrument_id"]] = (key, merged)
    return grouped


def summarize(receipt, mapping, *, require_order_ids=False):
    # The example uses the scenario's actual simulation clock (one second per
    # step), including quiet seconds, rather than timing wall-clock execution.
    seconds = receipt["simulation_time_ms"] // 1000
    segments = {leg: Segment(0, seconds) for leg in ("spot", "perp")}
    for row in aggregate_receipts(receipt["trade_receipts"], require_order_ids=require_order_ids):
        leg = "spot" if row["instrument_id"].endswith("SPOT") else "perp"
        if row["time_ms"] >= seconds * 1000:
            # Closed window [0, T): boundary trades belong to the next window.
            continue
        segments[leg].add(ReferenceTrade(row["time_ms"] * 1000,
            Decimal(row["price_tick"]) * Decimal(mapping["price_tick_usdt"]),
            Decimal(row["qty"]) * Decimal(mapping["quantity_lot_btc"]),
            row["taker_side"] == "Buy", row["underlying_trades"]))
    return {**{leg: segment.summary() for leg, segment in segments.items()},
            "pair": paired_summary(segments["spot"], segments["perp"])}


def compare(summary, reference):
    residuals = {}
    for leg in ("spot", "perp"):
        observed, synthetic = reference[leg], summary[leg]
        metrics = {}
        for name in ("aggregate_trades_per_second", "return_std_ppm_1s", "taker_side_persistence"):
            a, b = synthetic[name], observed[name]
            metrics[name] = {"synthetic": a, "reference": b,
                             "ratio": a / b if a is not None and b else None}
        metrics["median_trade_quantity"] = {"synthetic_btc": synthetic["aggregate_qty"]["p50"],
                                            "reference_btc": observed["aggregate_qty"]["p50"]}
        residuals[leg] = metrics
    return residuals


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", type=Path, default=ROOT / ".local/market-calibration")
    parser.add_argument("--example", type=Path, default=ROOT / "target/debug/examples/behavior_market.exe")
    parser.add_argument("--steps", type=int, default=600)
    parser.add_argument("--seeds", type=int, nargs="+", default=[7, 19, 41])
    args = parser.parse_args()
    directory = args.directory.resolve()
    reference = json.loads((directory / "reference.json").read_text(encoding="utf-8"))
    mapping = json.loads((directory / "mapping.json").read_text(encoding="utf-8"))
    reports = []
    for seed in args.seeds:
        path = directory / f"simulation-{seed}.json"
        with path.open("w", encoding="utf-8") as output:
            subprocess.run([str(args.example.resolve()), str(seed), str(directory / "recipe.json"), str(args.steps)],
                           stdout=output, check=True, timeout=300)
        receipt = json.loads(path.read_text(encoding="utf-8"))
        summary = summarize(receipt, mapping)
        reports.append({"seed": seed, "simulation_seconds": receipt["simulation_time_ms"] / 1000,
                        "trades": receipt["trades"], "summary": summary,
                        "fit_residuals": compare(summary, reference["fit"]),
                        "holdout_residuals": compare(summary, reference["holdout"])})
    result = {"source_day_utc": reference["day_utc"], "example_sha256": hashlib.sha256(args.example.read_bytes()).hexdigest(),
              "fit_seconds": reference["fit_seconds"], "holdout_seconds": reference["holdout_seconds"],
              "used_holdout_to_fit": False, "runs": reports,
              "qualification": "reference-scale recipe and measured residuals; not market-realism qualification",
              "limitations": ["new receipts use contiguous aggregation; legacy receipts without taker order IDs remain raw",
                  "perpetual 100ms buckets are a proxy on the one-second simulation clock; no intrasecond timing qualification",
                  "duration and population differ; rates use all elapsed seconds, including quiet seconds",
                  "only adjacent active-second returns are measured; sparse markets must also be assessed by active seconds",
                  "source trade data cannot identify realistic quotes, cancellations, news causality, latency or trader inventory"]}
    (directory / "comparison.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"runs": [{"seed": r["seed"], "trades": r["trades"]} for r in reports], "qualified_as_real_market": False}))


if __name__ == "__main__":
    main()
