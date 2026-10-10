"""Create a finite, mixed background market using the shared Rust-exported recipe."""
import argparse
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from marketforge import Client
from marketforge.batch import child_seed


def recipe(room="background-market", seed=7, autostart=True, pine=False):
    if not room.strip() or not 0 <= seed <= 2**64 - 1:
        raise ValueError("use a nonempty room id and an unsigned 64-bit seed")
    spec = json.loads((ROOT / "scripts/fixtures/background_market.json").read_text(encoding="utf-8"))
    spec["scenario"]["room_id"] = room
    spec["autostart_agents"] = autostart
    if pine:
        for index, (script, interval, inputs) in enumerate([
            ("ma.pine", 10000, {"Fast": 3, "Slow": 8}),
            ("ma.pine", 15000, {"Fast": 5, "Slow": 12}),
            ("rsi.pine", 10000, {"Length": 7, "Low": 35, "High": 65}),
            ("rsi.pine", 15000, {"Length": 10, "Low": 30, "High": 70}),
            ("breakout.pine", 10000, {"Length": 6}),
            ("breakout.pine", 15000, {"Length": 10}),
        ]):
            account = 300 + index
            spec["scenario"]["accounts"].append({"Spot": {
                "account_id": account, "cash_balance": 10000, "position_qty": 0,
            }})
            spec["agents"].append({"Plugin": {
                "participant": {"participant_id": f"pine-{index + 1}", "kind": "RuleAgent",
                                "room_id": room, "account_id": account, "instrument_id": "V-BTC-SPOT"},
                "plugin_id": "pine.strategy", "plugin_version": "1.0.0", "state_version": 1,
                "config_version": 1, "config": {"script": script, "inputs": inputs,
                    "bar_interval_ms": interval, "history_limit": 512, "max_qty": 2,
                    "inventory_cap": 10, "slippage_ticks": 2, "fee_buffer_ppm": 1000},
            }})
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        bot["participant"]["room_id"] = room
        bot["seed"] = max(1, child_seed(seed, bot["participant"]["participant_id"]) & (2**53 - 1))
    return spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:57305")
    parser.add_argument("--room", default="background-market")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--dry-run", action="store_true", help="print the payload without creating a room")
    parser.add_argument("--pine", action="store_true", help="add six Pine bots with exclusive flat accounts")
    args = parser.parse_args()
    spec = recipe(args.room, args.seed, pine=args.pine)
    if args.dry_run:
        print(json.dumps(spec, ensure_ascii=False, indent=2))
        return
    client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
    if args.pine and "pine.strategy" not in {item["id"] for item in client.list_bots()}:
        raise RuntimeError("pine.strategy is not installed; enable the plugin directory and restart the server")
    result = client._request("POST", "/rooms", spec)
    print(json.dumps({"room": args.room, "seed": args.seed, "background_traders": len(spec["agents"]),
                      "autostart": spec["autostart_agents"], "result": result}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
