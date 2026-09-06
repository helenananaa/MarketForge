#!/usr/bin/env python3
"""Minimal strategy.v1 example: buy remaining training qty at the visible ask."""

from __future__ import annotations

import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from marketforge import Client, MarketForgeError


def main() -> int:
    if len(sys.argv) < 4:
        print("usage: buy_remaining.py BASE_URL ROOM_ID ACCOUNT_ID [QTY]", file=sys.stderr)
        return 2
    client = Client(sys.argv[1], trusted_owner_urls=[sys.argv[1].rstrip("/")])
    room_id = sys.argv[2]
    account_id = int(sys.argv[3])
    qty = int(sys.argv[4]) if len(sys.argv) > 4 else 1
    try:
        observed = client.observe(room_id, account_id)
        asks = observed["observation"]["book"]["asks"]
        if not asks:
            print(json.dumps({"ok": False, "error": "empty ask", "cursor": client.cursor}))
            return 1
        price = asks[0]["price_tick"]
        result = client.place(
            room_id,
            account_id,
            "buy",
            price,
            qty,
            idempotency_key=f"buy-remaining-{room_id}-{account_id}-{client.cursor}",
        )
        print(json.dumps({"ok": True, "cursor": client.cursor, "order": result}, indent=2))
        return 0
    except MarketForgeError as exc:
        print(json.dumps({"ok": False, "error": str(exc), "cursor": client.cursor}))
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
