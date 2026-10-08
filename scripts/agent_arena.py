"""Provision a finite-money local arena and grant each AI only its own account.

This is an operator command, never a model tool. Does not invoke a model or start trading.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from marketforge import Client
from marketforge.agents.runtime import identifier


def arena(room, names, cash=100000):
    def instrument(name):
        return {"instrument_id": name, "symbol": name, "venue_id": "default-venue",
                "base_asset": "V", "quote_asset": "USD", "tick_size": 1, "lot_size": 1}
    def order(oid, side, price, qty):
        return {"NewOrder": {"order_id": oid, "account_id": 10, "side": side,
                "kind": {"Limit": {"price_tick": price}}, "qty": qty, "reduce_only": False}}
    return {"room_id": room,
            "market": {"Spot": {"instrument": instrument("V-USD-SPOT"),
                "clearing": {"maker_fee_ppm": 100, "taker_fee_ppm": 300}, "risk": {"allow_short": False}}},
            "extra_markets": [{"Perp": {"instrument": instrument("V-USD-PERP"),
                "clearing": {"maker_fee_ppm": 100, "taker_fee_ppm": 300, "leverage": 2},
                "risk": {}, "initial_mark_price_tick": 100}}],
            "accounts": [{"Spot": {"account_id": 10, "cash_balance": cash * 2, "position_qty": 1000}}] +
                [{"Basic": {"account_id": 20 + index, "cash_balance": cash}} for index, _ in enumerate(names)],
            "seed_orders": [order(1, "Buy", 99, 100), order(2, "Sell", 101, 100)],
            "routed_seed_orders": [{"instrument_id": "V-USD-PERP", "command": order(3, "Buy", 99, 100)},
                                   {"instrument_id": "V-USD-PERP", "command": order(4, "Sell", 101, 100)}]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:57305")
    parser.add_argument("--room", default="ai-arena")
    parser.add_argument("--traders", nargs="+", default=["trader-1", "trader-2"])
    parser.add_argument("--cash", type=int, default=100000)
    args = parser.parse_args()
    names = [identifier(name) for name in args.traders]
    if len(set(names)) != len(names) or not 1 <= len(names) <= 16 or not 1000 <= args.cash <= 10**9:
        raise ValueError("use 1-16 unique trader names and cash between 1000 and 1 billion")
    client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
    client._request("POST", "/rooms", {"scenario": arena(args.room, names, args.cash), "autostart_agents": False})
    for index, name in enumerate(names):
        client.add_member(args.room, "agent-" + name, "trader")
        client.assign_account(args.room, 20 + index, "agent-" + name)
    # Explicitly enable only the market clock; LLMs live in the plugin service.
    client.start_agents(args.room, [], interval_ms=1000)
    print(json.dumps({"room": args.room, "instruments": ["V-USD-SPOT", "V-USD-PERP"],
                      "traders": [{"id": name, "account_id": 20 + i, "user_id": "agent-" + name} for i, name in enumerate(names)],
                      "note": "Seeded finite liquidity only. No automatic spot/perp price linkage or background population added."}, indent=2))


if __name__ == "__main__":
    main()
