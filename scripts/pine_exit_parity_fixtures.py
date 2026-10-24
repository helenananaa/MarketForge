"""Generate exit/protection vectors using the unchanged Python broker oracle."""
import copy
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python/tests"))
from test_pine_bot import bot
from test_pine_broker import broker_request, perp


def cases():
    broker = bot.broker_module()
    rows = []

    def add(name, intents, *, position=7, price=100, tick=1, lot=1,
            quantize=True, state=None, protections=None, spot=False, entries=None):
        req = broker_request([100])
        if not spot:
            req = perp(req, position=position, avg=100)
        config, view = req["config"], req["observation"]
        config.update(quantize_strategy_orders=quantize, max_qty=30, inventory_cap=100)
        view["instrument"] = {"tick_size": tick, "lot_size": lot}
        view["public_trades"] = [] if price is None else [{"price_tick": price}]
        view["book"] = {"asks": [{"price_tick": 102, "qty": 100}], "bids": [{"price_tick": 98, "qty": 100}]}
        view["position_protections"] = protections or []
        instrument = view["instrument_id"]
        if state is None:
            state = {"identity": "exit-fixture", "orders": {},
                     "entries": {instrument + "::L": {"qty": position if entries is None else entries}}}
        # A request crosses serde_json's ordered object boundary before dispatch.
        state = json.loads(json.dumps(state, sort_keys=True))
        row = {"name": name, "intents": intents, "views": [view], "config": config, "state": state}
        working, settings = copy.deepcopy(state), copy.deepcopy(config)
        try:
            actions = broker.translate(intents, 2, {instrument: view}, instrument, settings, working)
            actions += broker.replacements(working, {instrument: view}, settings)
            wakes = []
            actions += broker.evaluate_exits(working, {instrument: view}, settings, wakes)
            working.pop("_wake_prices", None)
            levels = {}
            for market, direction, price in wakes:
                level = levels.setdefault(market, {"instrument_id": market})
                field, pick = ("at_or_above", min) if direction > 0 else ("at_or_below", max)
                level[field] = pick(level.get(field, price), price)
            if levels:
                working["_wake_prices"] = [levels[k] for k in sorted(levels)]
            row["expected"] = {"actions": actions, "state": working}
        except (ValueError, TypeError, KeyError, OverflowError):
            row["error"] = True
        rows.append(row)
        return row.get("expected", {}).get("state")

    def intent(**kw):
        return {"action": "exit", "id": "X", **kw}

    for pos in (0, 7, -7):
        for tick in (1, 2):
            for fields in ({"limit": 110.3, "stop": 90.3}, {"profit": 5.25, "loss": 6.25},
                           {"limit": 120, "profit": 1, "stop": 80, "loss": 1},
                           {"trail_price": 100.4, "trail_offset": 2.25},
                           {"trail_points": 4.25, "trail_offset": 3.25}, {}):
                for price in (None, 80, 100, 120):
                    add(f"grid-{pos}-{tick}-{fields}-{price}", [intent(**fields)], position=pos, price=price, tick=tick, lot=2)
        for qty in ({"qty": 0.3}, {"qty": 2.8}, {"qty_percent": 33.3}, {"qty_percent": 100.1}, {"qty_percent": True}):
            add(f"qty-{pos}-{qty}", [intent(profit=5, **qty)], position=pos)
        for entry in (None, "L", "Missing", "other::L", ""):
            add(f"owner-{pos}-{entry}", [intent(profit=10, from_entry=entry)], position=pos, entries=-pos)
    for quantize in (True, False):
        for fields in ({"stop": 90.5}, {"limit": -10}, {"limit": True}, {"limit": "110"},
                       {"profit": -5}, {"trail_price": 90, "trail_offset": 0}, {"trail_price": 90}):
            add(f"invalid-{quantize}-{fields}", [intent(**fields)], tick=2, quantize=quantize)
    add("spot-refused", [intent(profit=5)], spot=True)
    add("exit-insertion-order", [intent(id="Z", limit=120), intent(id="A", limit=130)])
    add("exit-update-preserves-order", [intent(id="Z", limit=120), intent(id="A", limit=130), intent(id="Z", limit=140)])
    add("cancel-reinsert-order", [intent(id="Z", limit=120), intent(id="A", limit=130), {"action":"cancel", "id":"Z"}, intent(id="Z", limit=140)])
    add("shared-reduction-budget", [{"action":"close", "id":"L", "qty":3}, intent(id="Z", limit=90), intent(id="A", limit=80)])
    installed = add("install", [intent(profit=10, loss=10)])
    key = next(iter(installed["exit_specs"]))
    spec = installed["exit_specs"][key]
    protection = {"position_side":spec["position_side"], "spec":spec["installed"], "status":"armed"}
    for status in ("pending", "accepted", "rejected"):
        saved = copy.deepcopy(installed)
        saved["orders"][key].update(pending=status == "pending", rejected=status == "rejected")
        add("retry-"+status, [], state=saved, protections=[protection])
        add("replace-"+status, [intent(profit=20)], state=saved, protections=[protection])
        add("cancel-"+status, [{"action":"cancel", "id":"X"}], state=saved, protections=[protection])
        add("cancel-all-"+status, [{"action":"cancel_all", "id":""}], state=saved, protections=[protection])
        add("latched-"+status, [intent(profit=20)], state=saved, price=120, protections=[{**protection,"status":"triggered"}])
        add("flat-"+status, [], state=saved, position=0)
    add("cancel-foreign-protection", [{"action":"cancel", "id":"X"}], state=installed,
        protections=[{**protection,"spec":{**protection["spec"], "exit_qty":1}}])
    trailing = add("trail-activation", [intent(trail_price=105, trail_offset=2.3)], price=106, tick=2)
    add("trail-stays-active", [], state=trailing, price=100, tick=2)
    add("trail-updated", [intent(trail_price=120, trail_offset=3.3)], state=trailing, price=100, tick=2)
    add("trail-flat-reset", [], state=trailing, position=0, tick=2)
    return rows


if __name__ == "__main__":
    rows = cases()
    path = ROOT / "python/tests/fixtures/pine_exit_direct.json"
    path.write_text(json.dumps(rows, indent=2) + "\n", encoding="utf-8")
    print(f"{len(rows)} vectors: {path}")
