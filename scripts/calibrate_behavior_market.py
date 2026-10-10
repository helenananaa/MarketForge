"""Build a spot/perpetual statistical reference and a finite native recipe.

Only the first half of the UTC window influences the recipe. The second half
is held out for comparison. Trade archives do not establish executable spreads.
"""
import argparse
from decimal import Decimal
import json
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python"))
from marketforge.calibration import acquire, day_start, reference_profile
from behavior_market import behavior_recipe


def fitted_recipe(profile, room="reference-market", seed=7):
    if profile["symbol"] != "BTCUSDT":
        raise ValueError("the current spot/perpetual recipe maps BTCUSDT only")
    spec = behavior_recipe(room, seed)
    fit = profile["fit"]
    anchor = 1_000_000
    quantity_lot = Decimal("0.001")
    source_price = Decimal(fit["spot"]["first_price"])
    price_unit = source_price / anchor
    centers = {"spot": anchor, "perp": round(Decimal(fit["perp"]["first_price"]) / price_unit)}
    fee_ppm = spec["scenario"]["market"]["Spot"]["clearing"]["maker_fee_ppm"]
    def move(leg, period, quantile="p95"):
        value = fit[leg][f"absolute_return_ppm_{period}s"][quantile] or 0
        return max(1, round(value * centers[leg] / 1_000_000))
    def spread(leg):
        # This is a synthetic quote width, with the model's fee floor, not a
        # bid/ask spread observed in the aggregate-trade file.
        return max(2, (centers[leg] * fee_ppm + 999_999) // 1_000_000 + move(leg, 1, "p50"))
    cash_scale = anchor // 100
    for account in spec["scenario"]["accounts"]:
        data = next(iter(account.values()))
        if "cash_balance" in data:
            data["cash_balance"] *= cash_scale
    perp = spec["scenario"]["extra_markets"][0]["Perp"]
    perp["initial_mark_price_tick"] = centers["perp"]
    first_funding = profile["funding_day"][0]
    if first_funding["time_ms"] >= (day_start(profile["day_utc"]) + profile["fit_seconds"]) * 1000:
        raise ValueError("no observed funding rate in the fit window; refusing future funding")
    perp["funding"]["interval_ms"] = first_funding["interval_hours"] * 3_600_000
    perp["funding"]["base_rate_ppm"] = round(Decimal(first_funding["rate"]) * 1_000_000)
    for order in spec["scenario"]["seed_orders"]:
        data = order["NewOrder"]
        data["kind"]["Limit"]["price_tick"] = anchor + (spread("spot") if data["side"] == "Sell" else -spread("spot"))
    values = [a for a in spec["agents"] if a["Plugin"]["plugin_id"] == "ValueTrader"]
    for index, agent in enumerate(values):
        agent["Plugin"]["config"]["fair_price_tick"] = anchor + round((index - (len(values) - 1) / 2) * move("spot", 60))
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        leg = "spot" if bot["participant"]["instrument_id"].endswith("SPOT") else "perp"
        config = bot["config"]
        config["fallback_price_tick"] = centers[leg]
        if "fair_price_tick" in config and bot["plugin_id"] != "ValueTrader":
            config["fair_price_tick"] = centers[leg]
        if bot["plugin_id"] in ("ExecutionTrader", "PovExecutionTrader", "BasisArbitrageTrader"):
            config["max_slippage_ticks"] = max(3, move(leg, 1) * 2)
        if config.get("risk"):
            config["risk"]["max_volatility_ticks"] = max(spread(leg) * 4, move(leg, 1, "p99") * 8)
        if bot["plugin_id"] == "DynamicMarketMaker":
            config.update(half_spread_ticks=spread(leg), inventory_skew_ticks=min(5, move(leg, 1)),
                          level_spacing_ticks=spread(leg), withdraw_volatility_ticks=max(spread(leg) * 4, move(leg, 1, "p99") * 4))
        if bot["plugin_id"] == "ValueTrader":
            config["edge_ticks"] = spread(leg)
        if bot["plugin_id"] == "TrendTrader":
            config["signal_threshold_ticks"] = move(leg, 1)
        if bot["plugin_id"] == "AdaptiveNoiseTrader":
            persistence = fit[leg]["taker_side_persistence"]
            # The non-persistent branch chooses a fresh random side, which is
            # still the previous side half of the time: P(same) = (1 + p) / 2.
            config["side_persistence_ppm"] = round(max(0, min(1, 2 * persistence - 1)) * 1_000_000) if persistence is not None else 0
            config["price_radius_ticks"] = move(leg, 1)
            config["max_qty"] = max(1, min(1000, round(Decimal(str(fit[leg]["aggregate_qty"]["p50"])) / quantity_lot)))
            config["inventory_cap"] = max(config.get("inventory_cap", 100), config["max_qty"] * 10)
        if bot["plugin_id"] == "BasisArbitrageTrader":
            # The strategy still checks executable books and its round-trip
            # fees; a positive last-trade difference alone never enters a pair.
            config["entry_basis_ticks"] = max(1, round(max(0, fit["pair"]["last_trade_basis_ppm"]["p95"] or 0) * anchor / 1_000_000))
            config["exit_basis_ticks"] = 0
    for event in spec["scenario"]["market_events"]:
        event["impact_ticks"] = move("spot", 60) * (1 if event["impact_ticks"] > 0 else -1)
        event["headline"] = "合成消息：按历史一分钟价格变化尺度施加的需求冲击"
    mapping = {"reference_symbol": profile["symbol"], "source_day_utc": profile["day_utc"],
               "price_tick_usdt": str(price_unit), "quantity_lot_btc": str(quantity_lot),
               "cash_unit_usdt": str(price_unit * quantity_lot), "spot_anchor_tick": anchor,
               "fit_seconds": profile["fit_seconds"], "used_holdout_to_fit": False,
               "synthetic_quote_half_spread_ticks": {leg: spread(leg) for leg in centers},
               "assumptions": ["internal V/BTC accounting names stay normalized; mapping is a statistical reference",
                   "model fees, latency, activity, inventory targets, funding cap and participant count remain assumptions",
                   "funding uses the first selected day's observed rate and interval, not the full historical rate schedule",
                   "quote width is inferred from trade volatility plus model fees; no historical order book is available",
                   "news is synthetic and does not identify real news causes"]}
    return spec, mapping


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--symbol", choices=["BTCUSDT"], default="BTCUSDT")
    parser.add_argument("--day", default="2025-01-02")
    parser.add_argument("--window-seconds", type=int, default=3600)
    parser.add_argument("--room", default="reference-market")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--output", type=Path, default=ROOT / ".local/market-calibration")
    parser.add_argument("--source-cache", type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    receipts = acquire(args.symbol, args.day, args.source_cache or args.output / "source")
    profile = reference_profile(receipts, args.day, args.window_seconds, args.symbol)
    recipe, mapping = fitted_recipe(profile, args.room, args.seed)
    for name, value in (("reference.json", profile), ("recipe.json", recipe), ("mapping.json", mapping)):
        (args.output / name).write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"source_rows": profile["validated_daily_aggregate_rows"], "fit_seconds": profile["fit_seconds"],
                      "holdout_seconds": profile["holdout_seconds"], "bots": len(recipe["agents"])}, ensure_ascii=False))


if __name__ == "__main__":
    main()
