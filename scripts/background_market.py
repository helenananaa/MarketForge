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


def recipe(room="background-market", seed=7, autostart=True):
    if not room.strip() or not 0 <= seed <= 2**64 - 1:
        raise ValueError("use a nonempty room id and an unsigned 64-bit seed")
    spec = json.loads((ROOT / "scripts/fixtures/background_market.json").read_text(encoding="utf-8"))
    spec["scenario"]["room_id"] = room
    spec["autostart_agents"] = autostart
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
    args = parser.parse_args()
    spec = recipe(args.room, args.seed)
    if args.dry_run:
        print(json.dumps(spec, ensure_ascii=False, indent=2))
        return
    client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
    result = client._request("POST", "/rooms", spec)
    print(json.dumps({"room": args.room, "seed": args.seed, "background_traders": len(spec["agents"]),
                      "autostart": spec["autostart_agents"], "result": result}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
