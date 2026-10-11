"""Public event queries shared by MCP and durable event alerts."""
import urllib.parse

EVENT_TYPES = ("liquidation", "funding", "price_update", "trade")
EVENT_FILTERS = {
    "event_type": {"type": "string", "enum": list(EVENT_TYPES)},
    "min_notional": {"type": ["integer", "string"], "description": "Inclusive per-fill native quote amount: price_tick * qty; use decimal string for wide integers."},
    "side": {"type": "string", "enum": ["Buy", "Sell"]},
    "min_rate_ppm": {"type": "integer"}, "max_rate_ppm": {"type": "integer"},
}
EVENT_QUERY_FIELDS = {"instrument": {"type": "string"}, **EVENT_FILTERS,
    "after_command_seq": {"type": "integer"}, "from_start": {"type": "boolean"},
    "limit": {"type": "integer"}, "start_market_time_ms": {"type": "integer"},
    "end_market_time_ms": {"type": "integer"}}


def filters(args):
    from .runtime import integer
    query = {k: args[k] for k in EVENT_FILTERS if k in args}
    kind = query.get("event_type")
    if kind is not None and kind not in EVENT_TYPES:
        raise ValueError("invalid event_type")
    if "min_notional" in query or "side" in query:
        if kind != "liquidation": raise ValueError("notional/side filters require event_type=liquidation")
    if "side" in query and query["side"] not in ("Buy", "Sell"):
        raise ValueError("invalid liquidation fill side")
    if "min_notional" in query:
        value = query["min_notional"]
        if isinstance(value, str):
            if not value.isascii() or not value.isdecimal(): raise ValueError("min_notional must be a decimal integer")
            value = int(value)
        query["min_notional"] = str(integer(value, 0, 2**127-1))
    for field in ("min_rate_ppm", "max_rate_ppm"):
        if field in query:
            if kind != "funding": raise ValueError("rate filters require event_type=funding")
            query[field] = integer(query[field], -(2**31), 2**31-1)
    if query.get("min_rate_ppm", -(2**31)) > query.get("max_rate_ppm", 2**31-1):
        raise ValueError("invalid signed funding rate range")
    return query


def read_events(runtime, config, args):
    from .runtime import integer
    instrument = args["instrument"]
    runtime.check_instrument(config, instrument)
    query = {**filters(args), "limit": integer(args.get("limit", 100), 1, 500)}
    for field in ("after_command_seq", "start_market_time_ms", "end_market_time_ms"):
        if field in args: query[field] = integer(args[field], 0, 2**53-1)
    if "from_start" in args:
        if type(args["from_start"]) is not bool: raise ValueError("from_start must be boolean")
        query["from_start"] = args["from_start"]
    if query.get("from_start") and "after_command_seq" in query:
        raise ValueError("conflicting history cursors")
    if query.get("start_market_time_ms", 0) > query.get("end_market_time_ms", 2**53-1):
        raise ValueError("invalid market time range")
    room, market = (urllib.parse.quote(v, safe="") for v in (config["room"], instrument))
    data = runtime.client(config)._request("GET", f"/rooms/{room}/instruments/{market}/market-events", query=query)
    if not isinstance(data.get("events"), list) or type(data.get("latest_command_seq")) is not int or type(data.get("has_more")) is not bool:
        raise ValueError("exchange did not return public event history")
    return data
