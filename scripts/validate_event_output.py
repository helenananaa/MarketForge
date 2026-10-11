"""Real HTTP/SSE event checks. Run live, restart the server, then run replay.

Uses only the Python standard library. Creates uniquely named validation rooms;
does not alter existing rooms. The caller supplies a server and persistent state
path; database/server startup and shutdown remain under the caller's control.
"""
import argparse
import json
import queue
import threading
import time
import urllib.request
import urllib.error
import uuid
from pathlib import Path


class Client:
    def __init__(self, base_url):
        self.base_url = base_url.rstrip("/")

    def request(self, path, value=None, user="local-user"):
        data = None if value is None else json.dumps(value).encode()
        request = urllib.request.Request(
            self.base_url + path, data=data,
            headers={"content-type": "application/json", "x-user-id": user},
        )
        try:
            with urllib.request.urlopen(request, timeout=15) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise AssertionError(f"{path}: HTTP {error.code}: {error.read().decode()}") from error

    def assign(self, room, user, account):
        self.request(f"/rooms/{room}/members", {"user_id": user, "role": "trader"})
        self.request(f"/rooms/{room}/accounts/{account}/owners", {"user_id": user})

    def order(self, room, account, side, price, qty, instrument=None):
        path = f"/rooms/{room}"
        if instrument:
            path += f"/instruments/{instrument}"
        return self.request(path + "/orders", {
            "participant_id": "event-validation", "account_id": account,
            "action": {"PlaceLimit": {"side": side, "price_tick": price, "qty": qty}},
        })


class SSE:
    def __init__(self, client, room, scope, user="local-user", query=""):
        request = urllib.request.Request(
            f"{client.base_url}/rooms/{room}/stream/{scope}{query}",
            headers={"x-user-id": user},
        )
        self.response = urllib.request.urlopen(request, timeout=15)
        self.frames = queue.Queue()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            data = []
            while line := self.response.readline():
                line = line.decode().strip()
                if line.startswith("data:"):
                    data.append(line[5:].lstrip())
                elif not line and data:
                    self.frames.put(json.loads("\n".join(data)))
                    data = []
        except Exception as error:
            self.frames.put(error)

    def next(self):
        frame = self.frames.get(timeout=10)
        if isinstance(frame, Exception):
            raise frame
        return frame

    def until(self, predicate, limit=40):
        frames = []
        for _ in range(limit):
            frame = self.next()
            frames.append(frame)
            if predicate(frame):
                return frames
        raise AssertionError("Expected event not received")

    def close(self):
        # The reader can hold a buffered read lock until the socket timeout.
        # Closing asynchronously avoids making unrelated checks wait for it.
        threading.Thread(target=self.response.close, daemon=True).start()


def instrument(symbol):
    return {"symbol": symbol, "tick_size": 1, "lot_size": 1}


def spot(room):
    return {"room_id": room, "market": {"Spot": {
        "instrument": instrument("V-BTC-SPOT"),
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0}, "risk": {"allow_short": False},
    }}, "accounts": [
        {"Spot": {"account_id": 10, "cash_balance": 1000, "position_qty": 10}},
        {"Basic": {"account_id": 20, "cash_balance": 1000}},
    ], "seed_orders": [], "initial_portfolios": [
        {"account_id": 20, "balances": {"BTC": 500}},
    ], "venue_rules": {"transfers": {"deposit_delay_steps": 2, "withdrawal_delay_steps": 0}}}


def perp(room):
    return {"room_id": room, "market": {"Perp": {
        "instrument": instrument("V-BTC-PERP"),
        "clearing": {"maker_fee_ppm": 0, "taker_fee_ppm": 0, "leverage": 10,
                     "maintenance_margin_ppm": 50000, "liquidation_fee_ppm": 10000,
                     "auto_deleveraging_enabled": True},
        "risk": {}, "initial_mark_price_tick": 100,
    }}, "accounts": [
        {"Basic": {"account_id": account, "cash_balance": cash}}
        for account, cash in [(10, 10000), (20, 200), (30, 10000)]
    ], "seed_orders": []}


def accounting(frame, kind):
    return [event for event in frame.get("payload", {}).get("clearing_events", [])
            if event["type"] == kind]


def receipt(room, scope, user, snapshot, frames):
    return {"room": room, "scope": scope, "user": user,
            "after": snapshot["payload"]["cursor"]["command_seq"] or 0,
            "payloads": [frame["payload"] for frame in frames if frame["kind"] == "execution" and frame["command_seq"] > (snapshot["payload"]["cursor"]["command_seq"] or 0)]}


def live(client):
    prefix = "event-http-" + uuid.uuid4().hex[:12]
    room = prefix + "-spot"
    client.request("/rooms", spot(room))
    client.assign(room, "alice", 20)
    public, private = SSE(client, room, "public"), SSE(client, room, "private", "alice")
    initial, own = public.next(), private.next()
    assert own["payload"]["account_states"][0]["account"]["account_id"] == 20
    client.order(room, 10, "Sell", 100, 3)
    client.order(room, 10, "Sell", 101, 2)
    client.order(room, 20, "Buy", 101, 4)
    tape = [public.next() for _ in range(3)]
    fills = [private.next()]
    assert sum(len(frame["payload"]["trades"]) for frame in tape) == 2
    assert len(fills[0]["payload"]["trades"]) == 2
    assert "buyer_account_id" not in json.dumps(fills)
    assert '"account_id": 10' not in json.dumps(fills)
    book = {side: {level["price_tick"]: level["qty"] for level in initial["payload"]["book"][side]}
            for side in ["bids", "asks"]}
    for frame in tape:
        payload = frame["payload"]
        assert payload["book_delta_available"]
        assert "account_id" not in json.dumps(payload)
        for side in book:
            for level in payload["book_delta"][side]:
                if level["qty"]:
                    book[side][level["price_tick"]] = level["qty"]
                else:
                    book[side].pop(level["price_tick"], None)
    authority = client.request(f"/rooms/{room}/book")
    assert book == {side: {level["price_tick"]: level["qty"] for level in authority[side]} for side in book}
    client.request(f"/rooms/{room}/transfers/deposit", {"account_id": 20, "asset_id": "BTC", "amount": 100})
    pending = private.until(lambda f: f["kind"] == "wallet")[-1]
    assert pending["payload"]["events"][0]["status"] == "Pending"
    client.request(f"/rooms/{room}/clock/advance", {"steps": 2})
    done = private.until(lambda f: f["kind"] == "wallet")[-1]
    assert done["payload"]["events"][0]["portfolio_balance"]["total"] == "400"
    wallets = client.request(f"/rooms/{room}/wallet-events?wallet_cursor=0", user="alice")
    assert [e["status"] for e in wallets["events"]] == ["Pending", "Completed"]
    receipts = [receipt(room, "public", "local-user", initial, tape),
                receipt(room, "private", "alice", own, fills)]
    public.close()
    private.close()

    room = prefix + "-perp"
    client.request("/rooms", perp(room))
    client.order(room, 10, "Sell", 100, 10)
    client.order(room, 20, "Buy", 100, 10)
    client.order(room, 30, "Buy", 70, 4)
    client.assign(room, "short-owner", 10)
    public = SSE(client, room, "public")
    private = SSE(client, room, "private", "short-owner")
    initial, own = public.next(), private.next()
    client.request(f"/rooms/{room}/instruments/V-BTC-PERP/mark-price", {"price_tick": 99})
    healthy = private.until(lambda f: bool(accounting(f, "PerpMarginUpdated")))
    assert accounting(healthy[-1], "PerpMarginUpdated")[0]["account"]["margin_status"] == "healthy"
    client.request(f"/rooms/{room}/instruments/V-BTC-PERP/mark-price", {"price_tick": 70})
    partial = public.until(lambda f: bool(f["payload"]["liquidations"]))
    assert partial[-1]["payload"]["liquidations"][0]["qty"] == 4
    client.order(room, 30, "Buy", 70, 6)
    completed = public.until(lambda f: bool(f["payload"]["liquidations"]))
    assert completed[-1]["payload"]["liquidations"][0]["qty"] == 6
    adl = private.until(lambda f: bool(accounting(f, "PerpAutoDeleveraged")))
    assert accounting(adl[-1], "PerpAutoDeleveraged")[0]["loss"] > 0
    assert '"account_id": 20' not in json.dumps(healthy + adl)
    receipts.extend([receipt(room, "public", "local-user", initial, partial + completed),
                     receipt(room, "private", "short-owner", own, healthy + adl)])
    public.close()
    private.close()

    room = prefix + "-funding"
    scenario = json.loads((Path(__file__).parent / "fixtures/linked_market.json").read_text(encoding="utf-8"))["scenario"]
    scenario["room_id"] = room
    scenario["accounts"] = [{"Spot": {"account_id": a, "cash_balance": 1000000, "position_qty": 20}} for a in [10, 20]]
    scenario["seed_orders"] = []
    scenario["routed_seed_orders"] = []
    config = scenario["extra_markets"][0]["Perp"]
    config["initial_mark_price_tick"] = 10000
    config["clearing"].update(leverage=10, maker_fee_ppm=0, taker_fee_ppm=0)
    config["price_link"]["max_age_ms"] = 20000
    config["funding"] = {"interval_ms": 2000, "base_rate_ppm": 10000, "max_rate_ppm": 200000, "min_coverage_ppm": 900000}
    client.request("/rooms", scenario)
    client.order(room, 10, "Buy", 9999, 10)
    client.order(room, 10, "Sell", 10001, 10)
    client.order(room, 10, "Sell", 10000, 1, "V-BTC-PERP")
    client.order(room, 20, "Buy", 10000, 1, "V-BTC-PERP")
    client.order(room, 10, "Buy", 9999, 10, "V-BTC-PERP")
    client.order(room, 10, "Sell", 10001, 10, "V-BTC-PERP")
    client.assign(room, "funding-owner", 20)
    private = SSE(client, room, "private", "funding-owner")
    own = private.next()
    assert len(own["payload"]["markets"]) == 2
    client.request(f"/rooms/{room}/clock/advance", {"steps": 2})
    funding = private.until(lambda f: bool(accounting(f, "PerpFundingSettled")))
    assert accounting(funding[-1], "PerpFundingSettled")[0]["cash_delta"] == -100
    assert funding[-1]["payload"]["wallet_changes"]
    receipts.append(receipt(room, "private", "funding-owner", own, funding))
    private.close()
    return {"live_passed": True, "receipts": receipts, "wallet_room": prefix + "-spot", "wallets": wallets,
            "checks": ["full_fills", "depth_reconstruction", "account_isolation", "transfer_lifecycle",
                       "healthy_margin", "partial_liquidation", "adl", "funding", "multiple_instruments"]}


def replay(client, evidence):
    for record in evidence["receipts"]:
        query = "?after_command_seq=" + str(record["after"])
        stream = SSE(client, record["room"], record["scope"], record["user"], query)
        actual = [stream.next()["payload"] for _ in record["payloads"]]
        assert actual == record["payloads"], record["room"]
        stream.close()
    wallets = client.request(f'/rooms/{evidence["wallet_room"]}/wallet-events?wallet_cursor=0', user="alice")
    assert wallets == evidence["wallets"]
    stream = SSE(client, evidence["wallet_room"], "private", "alice", "?wallet_cursor=0")
    stream.next()  # Current snapshot.
    wallet = stream.until(lambda f: f["kind"] == "wallet")[-1]
    assert wallet["payload"]["events"] == wallets["events"]
    stream.close()
    evidence["replay_passed"] = True
    evidence["checks"].extend(["process_restart", "execution_replay_equality", "wallet_replay_equality"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--phase", choices=["live", "replay"], required=True)
    parser.add_argument("--state", type=Path, required=True)
    args = parser.parse_args()
    client = Client(args.base_url)
    started = time.monotonic()
    if args.phase == "live":
        evidence = live(client)
    else:
        evidence = json.loads(args.state.read_text(encoding="utf-8"))
        replay(client, evidence)
    evidence[args.phase + "_seconds"] = round(time.monotonic() - started, 3)
    args.state.parent.mkdir(parents=True, exist_ok=True)
    args.state.write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"phase": args.phase, "passed": True, "checks": evidence["checks"],
                      "seconds": evidence[args.phase + "_seconds"]}))


if __name__ == "__main__":
    main()
