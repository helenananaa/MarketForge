"""Generate reviewable broker vectors from the unchanged Python reference."""
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
    cases = []
    def add(name, intents, *, position=0, spot=False, tick=1, lot=1, quantize=True, orders=False):
        req = broker_request([100]) if spot else perp(broker_request([100]), position=position)
        config = req["config"]
        config.update(quantize_strategy_orders=quantize, max_qty=30, inventory_cap=100)
        view = req["observation"]
        view["instrument"] = {"tick_size": tick, "lot_size": lot}
        view["book"] = {"asks": [{"price_tick": 102, "qty": 100}], "bids": [{"price_tick": 98, "qty": 100}]}
        if spot:
            view["own_account"]["Spot"].update(position_qty=position, available_position=position)
        instrument = view["instrument_id"]
        state = {"identity": "parity-fixture", "orders": {}, "entries": {instrument + "::L": {"qty": position}}}
        if orders:
            for n, order_id, side in (("L", 51, "Buy"), ("Z", 52, "Sell")):
                key = broker.order_key(state, instrument, n)
                state["orders"][key] = {"instrument": instrument, "name": n, "side": side, "order_id": order_id, "pending": False, "filled_qty": 1, "entry": n}
                view["own_orders"].append({"order_id": order_id, "side": side, "price_tick": 90 if side == "Buy" else 110, "remaining_qty": 4})
        # The host's serde_json map order is the persisted wire order.
        state = json.loads(json.dumps(state, sort_keys=True))
        row = {"name": name, "intents": intents, "pyramiding": 2, "views": [view], "config": config, "state": state}
        working, settings = copy.deepcopy(state), copy.deepcopy(config)
        try:
            actions = broker.translate(intents, 2, {instrument: view}, instrument, settings, working)
            actions += broker.replacements(working, {instrument: view}, settings)
            row["expected"] = {"actions": actions, "state": working}
        except ValueError:
            row["error"] = True
        cases.append(row)
    for side in ("long", "short"):
        for qty in (0.8, 5.9, 6, 1e-10):
            for price in (None, 100.4):
                add(f"grid-{side}-{qty}-{price}", [{"action": "entry", "id": "L", "direction": side, "qty": qty, "limit": price}], tick=2, lot=2)
    for qty in (True, "5", 0, -1):
        add(f"invalid-qty-{qty}", [{"action": "entry", "id": "L", "direction": "long", "qty": qty}])
    for position in (7, -7):
        for intent in ({"qty": 2.9}, {"qty_percent": 33.3}, {}, {"qty_percent": 100.1}, {"qty_percent": True}):
            add(f"partial-close-{position}-{intent}", [{"action": "close", "id": "L", **intent}], position=position)
        add(f"close-all-{position}", [{"action": "close_all", "id": ""}], position=position, orders=True)
        add(f"duplicate-close-{position}", [{"action": "close", "id": "L"}] * 2, position=position)
        add(f"duplicate-all-{position}", [{"action": "close_all", "id": ""}] * 2, position=position)
    add("cancel-all", [{"action": "cancel_all", "id": ""}], orders=True)
    add("cancel-named", [{"action": "cancel", "id": "L"}], orders=True)
    add("reversal-lot-grid", [{"action": "entry", "id": "L", "direction": "short", "qty": 5.9}], position=7, lot=2)
    add("two-spot-buys-share-cash", [{"action": "entry", "id": n, "direction": "long", "qty": 30, "limit": 500.4} for n in ("A", "B")], spot=True, tick=2, lot=2)
    add("spot-close-all", [{"action": "close_all", "id": ""}], position=7, spot=True, lot=2)
    add("strict-fractional", [{"action": "entry", "id": "L", "direction": "long", "qty": 5.9}], quantize=False)
    add("amend-less-aggressive", [{"action": "entry", "id": "L", "direction": "long", "qty": 4.8, "limit": 88.8}], orders=True, tick=2)
    add("cancel-before-replacement", [{"action": "entry", "id": "L", "direction": "long", "qty": 6.8, "limit": 101.2}], orders=True, tick=2)
    add("zero-close-with-live-order", [{"action": "close", "id": "L", "qty": 0.8}], position=7, orders=True)
    return cases


if __name__ == "__main__":
    path = ROOT / "python/tests/fixtures/pine_broker_direct.json"
    path.parent.mkdir(exist_ok=True)
    path.write_text(json.dumps(cases(), indent=2) + "\n", encoding="utf-8")
    print(path)
