"""Create a spot market with an explicitly linked, independently matched perpetual."""
import argparse
import copy
import json
import os
from pathlib import Path

from background_market import Client, child_seed, recipe


def linked_recipe(room="linked-market", seed=7, autostart=True):
    spec = recipe(room, seed, autostart)
    scenario = spec["scenario"]
    spot = scenario["market"]["Spot"]
    instrument = copy.deepcopy(spot["instrument"])
    instrument.update(instrument_id="V-BTC-PERP", symbol="V-BTC-PERP")
    scenario["extra_markets"].append({"Perp": {
        "instrument": instrument, "clearing": {**spot["clearing"], "leverage": 2},
        "risk": {}, "initial_mark_price_tick": 100,
        "funding": {"interval_ms": 60000, "base_rate_ppm": 100, "max_rate_ppm": 10000, "min_coverage_ppm": 800000},
        "price_link": {"spot_instrument_id": spot["instrument"]["instrument_id"], "max_age_ms": 30000},
    }})
    # Makers own and refresh all perpetual quotes. Static seed orders would
    # obstruct post-only repricing when the spot index moves past their price.
    for index in range(3):
        account = 200 + index
        scenario["accounts"].append({"Basic": {"account_id": account, "cash_balance": 25000}})
        maker = copy.deepcopy(spec["agents"][index])
        bot = maker["Plugin"]
        name = f"perp-maker-{index}"
        bot["participant"].update(participant_id=name, account_id=account, instrument_id=instrument["instrument_id"])
        bot["config"].update(inventory_target=0, inventory_cap=30)
        bot["seed"] = max(1, child_seed(seed, name) & (2**53 - 1))
        spec["agents"].append(maker)
    return spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:57305")
    parser.add_argument("--room", default="linked-market")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--export", type=Path)
    args = parser.parse_args()
    spec = linked_recipe(args.room, args.seed)
    if args.export:
        args.export.write_bytes((json.dumps(spec, ensure_ascii=False, indent=2) + "\n").encode("utf-8"))
    elif args.dry_run:
        print(json.dumps(spec, ensure_ascii=False, indent=2))
    else:
        client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
        result = client._request("POST", "/rooms", spec)
        print(json.dumps({"room": args.room, "instruments": ["V-BTC-SPOT", "V-BTC-PERP"], "result": result}, indent=2))


if __name__ == "__main__":
    main()
