"""Qualify installed library Pine scripts for the broker's incremental session.

Each script runs the same synthetic lifecycle twice, once with full replay
and once with the incremental external session: one perpetual account, a
closed one-minute bar per call, every order filled at once at its price (or
the close) and acknowledged on the next call. A script qualifies only when
every response and every durable state is identical in both modes, over each
price path, without errors. This qualifies the incremental path against
replay for these inputs; it is not a trading or market qualification.

Writes a report and, with --write-qualified, adds the qualified source hashes
to bot-plugins/pine-strategy/incremental-qualified.json under "library/...".
"""
from __future__ import annotations

import argparse
import copy
import importlib.util
import json
import multiprocessing
import random
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "bot-plugins/pine-strategy"
INTERVAL = 60000
ACCOUNT = 300


def load_bot():
    sys.path.insert(0, str(PACKAGE / ".runtime/strategy-ecosystem"))
    spec = importlib.util.spec_from_file_location("pine_bot", PACKAGE / "bot.py")
    bot = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bot)
    return bot


def prices(seed, count, drift):
    rng, price, path = random.Random(seed), 100000, []
    for _ in range(count):
        price = max(1000, price + drift + rng.choice([-1, 1]) * rng.randint(0, 60))
        path.append(price)
    return path


class Lifecycle:
    """Host-side bookkeeping for one perpetual account."""

    def __init__(self, script, path, defaults):
        self.config = dict(defaults)
        self.config.update(script=script, execution_mode="broker", inputs={}, bar_interval_ms=INTERVAL,
                           history_limit=2048, quantize_strategy_orders=True, max_qty=2,
                           inventory_cap=100, slippage_ticks=30, fee_buffer_ppm=1000)
        self.prices, self.state, self.receipts = path, None, []
        self.fills, self.details = [], []
        self.position = self.avg = self.realized = 0
        self.cash, self.order_id, self.trade_id = 1_000_000, 5000, 1

    def request(self, bar):
        closes = self.prices[:bar]
        candles = [{"schema_version": 1, "open_time_ms": i * INTERVAL, "close_time_ms": (i + 1) * INTERVAL,
                    "open_tick": p, "high_tick": p + 5, "low_tick": p - 5, "close_tick": p, "volume": 3,
                    "quote_volume": 3 * p, "trades": 3, "is_final": True} for i, p in enumerate(closes)]
        last = closes[-1]
        unrealized = self.position * (last - self.avg) if self.position else 0
        state = copy.deepcopy(self.state)
        if state is not None:
            state["_host_receipts"] = copy.deepcopy(self.receipts)
        return {"protocol_version": "bot.v1", "plugin_id": "pine.strategy", "plugin_version": "2.0.0",
                "state_version": 1, "seed": 7, "config": copy.deepcopy(self.config), "state": state,
                "participant": {"participant_id": "qualify", "room_id": "qualify", "account_id": ACCOUNT,
                                "instrument_id": "V-BTC-PERP", "kind": "Strategy"},
                "observation": {"version": 1, "room_id": "qualify", "instrument_id": "V-BTC-PERP",
                    "status": "Running", "step": bar * 60, "market_time_ms": bar * INTERVAL,
                    "book": {"bids": [{"price_tick": last - 1, "qty": 100}],
                             "asks": [{"price_tick": last + 1, "qty": 100}]},
                    "public_trades": [{"price_tick": last}], "own_orders": [], "position_protections": [],
                    "own_account": {"Perp": {"account_id": ACCOUNT, "cash_balance": self.cash,
                        "available_cash": self.cash, "position_qty": self.position,
                        "avg_entry_price_tick": self.avg if self.position else 0,
                        "equity": self.cash + unrealized, "realized_pnl": self.realized,
                        "unrealized_pnl": unrealized, "fees_paid": 0, "funding_pnl": 0,
                        "margin_status": "Healthy"}},
                    "bot_market_data": {"interval_ms": INTERVAL, "candles": candles,
                        "own_fills": copy.deepcopy(self.fills), "fill_details": copy.deepcopy(self.details),
                        "truncated": False, "conditional_orders": []}}}

    def settle(self, actions, bar):
        last, self.receipts = self.prices[bar - 1], []
        for action in actions:
            order = action.get("BotOrder")
            if not order:
                continue
            kind, body = next(iter(order["action"].items()))
            receipt = {"key": order["key"], "instrument_id": order["instrument_id"], "action": order["action"],
                       "order_id": None, "command_seq": self.order_id, "market_time_ms": bar * INTERVAL,
                       "accepted": True, "error": None}
            if kind in ("PlaceProtected", "PlaceImmediateOrCancel", "PlaceReduceOnlyImmediateOrCancel"):
                self.order_id += 1
                receipt["order_id"] = self.order_id
                self.fill(body["qty"] * (1 if body["side"] == "Buy" else -1), body.get("price_tick") or last, bar)
            self.receipts.append(receipt)

    def fill(self, qty, price, bar):
        old = self.position
        if old and old * qty < 0:
            pnl = min(abs(old), abs(qty)) * (1 if old > 0 else -1) * (price - self.avg)
            self.realized += pnl
            self.cash += pnl
        new = old + qty
        if new == 0:
            self.avg = 0
        elif old == 0 or (old > 0) != (new > 0):
            self.avg = price
        elif abs(new) > abs(old):
            self.avg = (self.avg * abs(old) + price * abs(qty)) // abs(new)
        self.position = new
        self.fills.append({"trade_id": self.trade_id, "maker_account_id": 10, "taker_account_id": ACCOUNT,
                           "maker_order_id": 1, "taker_order_id": self.order_id,
                           "taker_side": "Buy" if qty > 0 else "Sell", "qty": abs(qty), "price_tick": price})
        self.details.append({"trade_id": self.trade_id, "market_time_ms": bar * INTERVAL, "fee_paid": 0})
        self.trade_id += 1


def lifecycle(bot, script, path, defaults, incremental):
    module = bot.incremental_module()
    module.SESSIONS.clear()
    module.qualified = (lambda source: True) if incremental else (lambda source: False)
    life, calls = Lifecycle(script, path, defaults), []
    for bar in range(1, len(path) + 1):
        encoded = []
        try:
            response = bot.decide(life.request(bar), borrow_history=True, encoded_response=encoded)
        except Exception as error:  # A failing script cannot qualify.
            return calls, f"bar {bar}: {error}"[:200]
        calls.append(encoded[0])
        life.state = json.loads(encoded[0])["state"]
        life.settle(response["actions"], bar)
    return calls, None


def qualify(job):
    index, original, bars, paths = job
    bot = load_bot()
    defaults = {k: v["default"] for k, v in json.loads((PACKAGE / "bot.json").read_text(encoding="utf-8"))["bot"]["parameters"].items()}
    script = original["installed_script"]
    source_sha = bot.incremental_module().source_sha256(bot.load_source(script))
    verdict = {"name": original["name"], "script": script, "source_sha256": source_sha, "bars": bars}
    for n, drift in enumerate(paths):
        path = prices(1000 + index * 10 + n, bars, drift)
        replay, replay_error = lifecycle(bot, script, path, defaults, False)
        session, session_error = lifecycle(bot, script, path, defaults, True)
        if replay_error or session_error:
            verdict.update(qualified=False, error=replay_error or session_error, path=n)
            return verdict
        if replay != session:
            first = next(i for i, (a, b) in enumerate(zip(replay, session)) if a != b)
            verdict.update(qualified=False, first_difference_bar=first + 1, path=n)
            return verdict
    verdict.update(qualified=True, calls=bars * len(paths))
    return verdict


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--manifest", type=Path, default=ROOT / "docs/strategy-library-originals.json")
    parser.add_argument("--bars", type=int, default=300)
    parser.add_argument("--workers", type=int, default=max(1, multiprocessing.cpu_count() - 4))
    parser.add_argument("--limit", type=int)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--write-qualified", action="store_true")
    parser.add_argument("--from-report", action="store_true",
                        help="write the qualified list from an existing --report without running")
    args = parser.parse_args()
    if args.from_report:
        report = json.loads(args.report.read_text(encoding="utf-8"))
        write_qualified([v for v in report["verdicts"] if v["qualified"]])
        return
    originals = json.loads(args.manifest.read_text(encoding="utf-8"))["qualified"][:args.limit]
    # A flat random walk and a trending path with reversals of momentum.
    jobs = [(i, original, args.bars, (0, 8)) for i, original in enumerate(originals)]
    with multiprocessing.Pool(args.workers) as pool:
        verdicts = []
        for verdict in pool.imap_unordered(qualify, jobs):
            verdicts.append(verdict)
            print(json.dumps({k: verdict.get(k) for k in ("name", "qualified", "error", "first_difference_bar")}), flush=True)
    verdicts.sort(key=lambda v: v["script"])
    qualified = [v for v in verdicts if v["qualified"]]
    report = {"scope": __doc__.split("\n\n")[1].replace("\n", " "), "bars_per_path": args.bars,
              "paths": ["random walk", "trend +8 ticks per bar"], "scripts": len(verdicts),
              "qualified": len(qualified), "verdicts": verdicts}
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps(report, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    print(json.dumps({"scripts": len(verdicts), "qualified": len(qualified)}))
    if args.write_qualified:
        write_qualified(qualified)


def write_qualified(qualified):
    path = PACKAGE / "incremental-qualified.json"
    current = json.loads(path.read_text(encoding="utf-8"))
    current["source_sha256"] = {k: v for k, v in current["source_sha256"].items() if not k.startswith("library/")}
    current["source_sha256"].update({"library/" + v["script"].rsplit("/", 1)[-1]: v["source_sha256"] for v in qualified})
    current["library_scope"] = ("library/* entries: scripts/qualify_library_incremental.py; replay and "
                                "incremental responses and states identical over its synthetic lifecycles")
    path.write_text(json.dumps(current, indent=2) + "\n", encoding="utf-8", newline="\n")
    print(json.dumps({"written": str(path), "library_entries": len(qualified)}))


if __name__ == "__main__":
    main()
