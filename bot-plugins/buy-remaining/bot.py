#!/usr/bin/env python3
"""Dependency-free bot.v1 example. One request, one response, then exit.

The platform is the only order submitter. All strategy state is returned as JSON;
restarting the Python process or the server does not reset the strategy.
"""
import json
import sys


def decide(request):
    if request["protocol_version"] != "bot.v1":
        raise ValueError("unsupported bot protocol")
    observation = request["observation"]
    account = observation["own_account"] or {}
    snapshot = next(iter(account.values()), {})
    position = int(snapshot.get("position_qty", 0))
    state = request["state"]
    # Count actual acquired inventory, not submitted orders: cancellations,
    # rejections and unfilled orders must not advance the purchase target.
    initial_position = position if state is None else int(state["initial_position"])
    acquired = max(0, position - initial_position)
    remaining = max(0, int(request["config"]["target_qty"]) - acquired)
    asks = observation["book"]["asks"]
    actions = []
    if asks and remaining and not observation["own_orders"]:
        top = asks[0]
        qty = min(remaining, int(top["qty"]), int(request["config"]["qty_per_step"]))
        if qty:
            actions.append({"PlaceImmediateOrCancel": {
                "side": "Buy", "price_tick": int(top["price_tick"]), "qty": qty,
            }})
    return {
        "protocol_version": "bot.v1",
        "plugin_id": request["plugin_id"],
        "plugin_version": request["plugin_version"],
        "state_version": request["state_version"],
        "actions": actions,
        "state": {"initial_position": initial_position, "observed_steps": int((state or {}).get("observed_steps", 0)) + 1},
    }


if __name__ == "__main__":
    print(json.dumps(decide(json.loads(sys.stdin.readline())), separators=(",", ":")), flush=True)
