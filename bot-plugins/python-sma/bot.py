"""First-party spot SMA example with shared public calculations and real inventory.

This is a new bounded strategy, not an equivalent port of a community framework.
"""
from functools import lru_cache
import hashlib
import json
import sys


def fingerprint(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


@lru_cache(maxsize=128)
def averages(prices, fast, slow):
    return sum(prices[-fast:]) / fast, sum(prices[-slow:]) / slow


def decide(request):
    if request["protocol_version"] != "bot.v1": raise ValueError("unsupported protocol")
    config, view, state = request["config"], request["observation"], dict(request["state"] or {})
    if config["fast"] >= config["slow"]: raise ValueError("fast must be below slow")
    data = view.get("bot_market_data")
    if not data or data["truncated"] or data["interval_ms"] != config["bar_interval_ms"]:
        raise ValueError("complete configured closed-bar history required")
    bars = data["candles"]
    if any(not b["is_final"] or b["close_time_ms"] > view["market_time_ms"] for b in bars):
        raise ValueError("forming/future bar is not visible")
    if any(a["open_time_ms"] >= b["open_time_ms"] for a, b in zip(bars, bars[1:])):
        raise ValueError("bar history is not strictly ordered")
    identity = fingerprint((request["participant"], request["seed"], config))
    if state and state["identity"] != identity: raise ValueError("strategy identity changed")
    prior = state.get("evaluated_bars", 0)
    if len(bars) < prior or (prior and fingerprint(bars[:prior]) != state["history_digest"]):
        raise ValueError("committed history changed or disappeared")
    account = view["own_account"].get("Spot")
    if account is None: raise ValueError("Python SMA requires a spot account")
    position = int(account["position_qty"])
    metadata = view.get("instrument") or {}
    tick, lot = int(metadata.get("tick_size", 1)), int(metadata.get("lot_size", 1))
    if tick < 1 or lot < 1: raise ValueError("positive tick and lot sizes required")
    actions = []
    if len(bars) > prior and len(bars) >= config["slow"] and not view["own_orders"]:
        fast, slow = averages(tuple(b["close_tick"] for b in bars[-config["slow"]:]), config["fast"], config["slow"])
        close = bars[-1]["close_tick"]
        asks, bids = view["book"]["asks"], view["book"]["bids"]
        if fast > slow and not position and asks:
            limit = min(asks[0]["price_tick"], close + config["slippage_ticks"] * tick)
            qty = min(config["qty"], config["inventory_cap"], int(account["available_cash"]) // (limit + 1))
            qty = qty // lot * lot
            if qty > 0: actions.append({"PlaceImmediateOrCancel":{"side":"Buy","price_tick":limit,"qty":qty}})
        elif fast < slow and position > 0 and bids:
            limit = max(bids[0]["price_tick"], close - config["slippage_ticks"] * tick)
            qty = min(position, int(account["available_position"]))
            qty = qty // lot * lot
            if qty > 0: actions.append({"PlaceImmediateOrCancel":{"side":"Sell","price_tick":limit,"qty":qty}})
    state.update(identity=identity, evaluated_bars=len(bars), history_digest=fingerprint(bars),
                 last_bar_time=bars[-1]["open_time_ms"] if bars else None,
                 observed_position=position, observed_time_ms=view["market_time_ms"])
    return {**{k:request[k] for k in ("protocol_version","plugin_id","plugin_version","state_version")}, "actions":actions,"state":state}


def serve():
    for line in sys.stdin:
        envelope = json.loads(line)
        if envelope.get("protocol") != "bot.worker.v1" or not isinstance(envelope.get("id"), int):
            raise ValueError("unsupported worker envelope")
        reply = {"protocol":"bot.worker.v1","id":envelope["id"]}
        try: reply["response"] = decide(envelope["request"])
        except Exception as error: reply["error"] = str(error)
        print(json.dumps(reply, separators=(",", ":"), allow_nan=False), flush=True)


if __name__ == "__main__":
    if sys.argv[1:] == ["--worker"]: serve()
    else: print(json.dumps(decide(json.loads(sys.stdin.readline())), separators=(",", ":"), allow_nan=False), flush=True)
