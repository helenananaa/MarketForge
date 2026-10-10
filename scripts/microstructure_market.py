"""Create the existing linked market with explicit microstructure and perp motives."""
import argparse
import json
import os
from pathlib import Path

from behavior_market import behavior_recipe
from background_market import Client, child_seed
from market_arrival_feedback import apply_feedback


def add_perp_motives(spec, seed=7):
    scenario = spec["scenario"]
    room = scenario["room_id"]
    perp = scenario["extra_markets"][0]["Perp"]
    perp["clearing"]["leverage"] = max(5, perp["clearing"]["leverage"])
    instruments = {a["Plugin"]["participant"]["instrument_id"] for a in spec["agents"]}
    instrument = next(i for i in instruments if i.endswith("PERP"))
    for index, (kind, leverage) in enumerate([
        ("FundingRateTrader", 1), ("FundingRateTrader", 2),
        ("LeveragedTrendTrader", 1), ("LeveragedTrendTrader", 2), ("LeveragedTrendTrader", 5)]):
        account = 500 + index
        if any(next(iter(a.values()))["account_id"] == account for a in scenario["accounts"]):
            raise ValueError("perpetual motive account already allocated")
        name = f"perp-motive-{index}"
        scenario["accounts"].append({"Basic": {"account_id": account, "cash_balance": 1000 + index * 250}})
        config = {"target_leverage": leverage, "inventory_cap": 150, "position_size": 100,
                  "max_qty": 10, "max_slippage_ticks": 5, "fee_buffer_ppm": 1000,
                  "decision_interval_ms": 1000 + index * 200, "jitter_ms": index * 150,
                  "risk": {"min_margin_buffer_ppm": 200000 + index * 50000,
                           "max_drawdown_ppm": 300000, "cooldown_ms": 10000 + index * 2000}}
        if kind == "FundingRateTrader":
            config.update(funding_entry_rate_ppm=300 + index * 200, funding_exit_rate_ppm=100,
                          funding_entry_window_ms=min(30000, perp["funding"]["interval_ms"]))
        else:
            config.update(lookback=3 + index * 2, signal_threshold_ticks=1 + index % 3)
        spec["agents"].append({"Plugin": {"participant": {
            "participant_id": name, "kind": "RuleAgent", "room_id": room,
            "account_id": account, "instrument_id": instrument},
            "plugin_id": kind, "plugin_version": "1", "state_version": 1, "config_version": 1,
            "seed": max(1, child_seed(seed, name) & (2**53 - 1)), "config": config}})


def microstructure_recipe(room="microstructure-market", seed=7, autostart=True, feedback=True):
    spec = behavior_recipe(room, seed, autostart)
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        if bot["plugin_id"] == "DynamicMarketMaker":
            bot["config"].update(book_pressure_ticks=2, book_pressure_levels=3,
                toxic_flow_threshold_ppm=800000, toxic_flow_min_qty=6,
                toxic_cooldown_ms=3000, recovery_ramp_ms=5000, requote_threshold_ticks=1)
    add_perp_motives(spec, seed)
    return apply_feedback(spec,seed=seed) if feedback else spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--room", default="microstructure-market")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--base-url", default="http://127.0.0.1:57305")
    parser.add_argument("--export", type=Path)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--periodic", action="store_true", help="export the prior recipe without arrival/depth feedback for comparable engine load measurements")
    args = parser.parse_args()
    spec = microstructure_recipe(args.room, args.seed, feedback=not args.periodic)
    if args.export:
        args.export.write_text(json.dumps(spec, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    elif args.dry_run:
        print(json.dumps(spec, ensure_ascii=False, indent=2))
    else:
        client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
        print(json.dumps(client._request("POST", "/rooms", spec), ensure_ascii=False))


if __name__ == "__main__":
    main()
