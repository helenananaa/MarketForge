"""Bounded public analytics; wide monetary quantities remain exact strings."""
import urllib.parse

WINDOW_FIELDS = {
    "instrument": {"type": "string"},
    **{k: {"type": "integer"} for k in ("window_ms", "end_market_time_ms", "scan_limit", "bucket_ms", "price_group_tick", "book_levels")},
    "large_trade_min_notional": {"type": ["integer", "string"]},
}
WINDOW_METRICS = ("trade_count", "volume_qty", "quote_volume", "buy_volume_qty", "sell_volume_qty",
    "buy_quote_volume", "sell_quote_volume", "delta_qty", "delta_quote", "vwap_tick", "first_price_tick",
    "last_price_tick", "price_change_ppm", "liquidation_count", "liquidation_notional",
    "long_liquidation_notional", "short_liquidation_notional", "liquidation_volume_qty",
    "large_trade_count", "large_trade_notional", "funding_receipt_count", "funding_settled_count",
    "funding_skipped_count", "funding_total_transfer", "last_funding_rate_ppm", "book_imbalance_ppm",
    "book_bid_qty", "book_ask_qty")


def query_args(args):
    from .runtime import integer
    bounds = {"window_ms": (1, 86400000), "end_market_time_ms": (0, 2**53-1),
        "scan_limit": (1, 100000), "bucket_ms": (1, 86400000), "price_group_tick": (1, 2**63-1), "book_levels": (1, 100)}
    query = {k: integer(args[k], *bound) for k, bound in bounds.items() if k in args}
    if "large_trade_min_notional" in args:
        value = args["large_trade_min_notional"]
        if isinstance(value, str):
            if not value.isascii() or not value.isdecimal(): raise ValueError("large_trade_min_notional must be a decimal integer")
            value = int(value)
        query["large_trade_min_notional"] = str(integer(value, 0, 2**127-1))
    if query.get("window_ms", 30000) // query.get("bucket_ms", max(1000, (query.get("window_ms", 30000)+510)//511)) >= 512:
        raise ValueError("at most 512 time buckets")
    return query


def read_window(runtime, config, args, detailed=False):
    instrument = args["instrument"]
    runtime.check_instrument(config, instrument)
    query = query_args(args)
    room, market = (urllib.parse.quote(v, safe="") for v in (config["room"], instrument))
    path = "order-flow" if detailed else "window-stats"
    data = runtime.client(config)._request("GET", f"/rooms/{room}/instruments/{market}/{path}", query=query)
    if not isinstance(data.get("metrics"), dict) or type(data.get("complete")) is not bool or type(data.get("through_command_seq")) is not int:
        raise ValueError("exchange did not return window analytics")
    return data
