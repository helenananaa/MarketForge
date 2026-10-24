"""Run the existing HTTP/receipt load driver on the qualified ecosystem cohort.

Retain the recipe's native liquidity agents, funding, seeds and Pine sources.
Select one instance per admitted source and use one-second bars for this bounded
service probe. This is separate from the unchanged 3,300-bot research benchmark.
All remaining options are those of benchmark_strategy_runtime.py.
"""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import sys

import benchmark_strategy_runtime as driver
import bot_runtime_receipts

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--ecosystem-recipe", type=Path, required=True)
    args, remaining = parser.parse_known_args()
    original = json.loads(args.ecosystem_recipe.read_text(encoding="utf-8-sig"))
    qualified = json.loads((ROOT / "exchange-server/src/pine_inprocess/ecosystem-qualified.json").read_text(encoding="utf-8"))
    pine_owners = {a["Plugin"]["participant"]["account_id"] for a in original["agents"]
                   if a["Plugin"]["plugin_id"] == "pine.strategy"}
    seen, agents = set(), []
    for agent in original["agents"]:
        plugin = agent["Plugin"]
        if plugin["plugin_id"] == "pine.strategy":
            script = plugin["config"]["script"]
            if script not in qualified or script in seen:
                continue
            seen.add(script)
            plugin["config"].update(bar_interval_ms=1000, inputs={}, quantize_strategy_orders=True)
        agents.append(agent)
    original["agents"] = agents
    owners = {a["Plugin"]["participant"]["account_id"] for a in agents}
    original["scenario"]["accounts"] = [a for a in original["scenario"]["accounts"] if next(iter(a.values()))["account_id"] in owners or next(iter(a.values()))["account_id"] not in pine_owners]
    original["scenario"]["accounts"].append({"Spot": {"account_id":900000, "cash_balance":100000, "position_qty":100}})
    probe = copy.deepcopy(original["scenario"]["market"])
    probe["Spot"]["instrument"].update(instrument_id="LOAD-PROBE-SPOT", symbol="LOAD-PROBE-SPOT")
    original["scenario"]["extra_markets"].append(probe)

    def recipe(count, room, seed, staggered=False, template=None, **kwargs):
        if count != len(seen) or seed != 7 or staggered or template is not None:
            raise ValueError(f"frozen ecosystem probe requires --counts {len(seen)} and seed 7")
        spec = copy.deepcopy(original)
        spec["scenario"]["room_id"] = room
        for agent in spec["agents"]:
            agent["Plugin"]["participant"]["room_id"] = room
        return spec
    driver.pine_recipe = recipe
    driver.normalized_pine_config = lambda plugin: copy.deepcopy(plugin["config"])
    # Arbitrary ecosystem lookbacks are not the bundled MA/RSI defaults.
    # This bounded service probe cannot certify all strategy branches or warmups.
    driver.lookbacks_exercised = lambda scheduler, language: None
    # Attribute coverage to the actual cohort, not the driver's three bundled
    # scripts. A short probe still does not require or certify sustained coverage.
    bot_runtime_receipts.AcceptedWindowCoverage.SOURCES = tuple(sorted(seen))
    sys.argv = [sys.argv[0], *remaining]
    driver.main()
    if "--export-only" in remaining:
        return
    output = Path(remaining[remaining.index("--output") + 1])
    for report_path in [output / "report.json", output / f"bots-{len(seen)}/report.json"]:
        report = json.loads(report_path.read_text(encoding="utf-8"))
        scope = f"{len(seen)} exact ecosystem Pine sources plus {len(agents)-len(seen)} original native liquidity agents; one-second bars; short in-memory-journal HTTP probe, not production capacity"
        report["scope"] = scope
        report["ecosystem_recipe_sha256"] = hashlib.sha256(args.ecosystem_recipe.read_bytes()).hexdigest()
        for case in report.get("cases", []):
            case["scope"] = scope
        report_path.write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
