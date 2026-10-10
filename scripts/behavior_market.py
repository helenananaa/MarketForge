"""Create a finite linked market with arbitrage, risk exits, POV and common news."""
import argparse
import json
import os
from pathlib import Path

from linked_market import linked_recipe
from background_market import Client, child_seed


def behavior_recipe(room="behavior-market", seed=7, autostart=True):
    spec = linked_recipe(room, seed, autostart)
    scenario = spec["scenario"]
    scenario["market_events"] = [
        {"id": "news-up", "instrument_id": "V-BTC-SPOT", "published_at_ms": 45000,
         "expires_at_ms": 110000, "impact_ticks": 18, "headline": "仿真事件：需求预期上调"},
        {"id": "news-down", "instrument_id": "V-BTC-SPOT", "published_at_ms": 160000,
         "expires_at_ms": 220000, "impact_ticks": -25, "headline": "仿真事件：风险偏好下降"},
    ]
    risk = {"max_drawdown_ppm": 150000, "trailing_stop_ppm": 180000,
            "max_volatility_ticks": 25, "cooldown_ms": 15000}
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        if bot["plugin_id"] != "ExecutionTrader":
            bot["config"]["risk"] = dict(risk)
        if bot["participant"]["instrument_id"] == "V-BTC-PERP":
            # Inventory pressure can produce an executable premium; the index
            # anchors valuation without forcing every quote to the same center.
            bot["config"]["inventory_skew_ticks"] = 10

    def add(name, account, instrument, plugin, config):
        spec["agents"].append({"Plugin": {
            "participant": {"participant_id": name, "kind": "RuleAgent", "room_id": room,
                            "account_id": account, "instrument_id": instrument},
            "plugin_id": plugin, "plugin_version": "1", "state_version": 1, "config_version": 1,
            "seed": max(1, child_seed(seed, name) & (2**53 - 1)), "config": config,
        }})

    for i in range(2):
        account = 430 + i
        scenario["accounts"].append({"Basic": {"account_id": account, "cash_balance": 25000}})
        add(f"perp-flow-{i}", account, "V-BTC-PERP", "AdaptiveNoiseTrader", {
            "inventory_cap": 24, "max_qty": 3, "activity_ppm": 700000,
            "side_persistence_ppm": 850000, "market_order_ratio_ppm": 850000,
            "price_radius_ticks": 10, "decision_interval_ms": 1250 + i * 500,
            "jitter_ms": 500, "risk": dict(risk),
        })

    # Two legs share an account but each exclusively manages one instrument.
    scenario["accounts"].append({"Spot": {"account_id": 400, "cash_balance": 40000, "position_qty": 0}})
    for leg, instrument, peer in [("Spot", "V-BTC-SPOT", "V-BTC-PERP"), ("Perp", "V-BTC-PERP", "V-BTC-SPOT")]:
        add(f"basis-{leg.lower()}", 400, instrument, "BasisArbitrageTrader", {
            "leg": leg, "hedge_instrument_id": peer, "entry_basis_ticks": 3, "exit_basis_ticks": 0,
            "position_size": 10, "inventory_cap": 10, "max_qty": 2, "fee_buffer_ppm": 1000,
            "hedge_timeout_ms": 7000, "decision_interval_ms": 1000, "jitter_ms": 0,
        })
    for i in range(4):
        account = 410 + i
        scenario["accounts"].append({"Spot": {"account_id": account, "cash_balance": 20000, "position_qty": 20}})
        add(f"event-{i}", account, "V-BTC-SPOT", "MarketEventTrader", {
            "inventory_target": 20, "inventory_cap": 50, "position_size": 15, "max_qty": 2,
            "fair_price_tick": 100, "information_delay_ms": i * 3000,
            "confidence_ppm": 400000 + i * 200000, "crowd_strength_ppm": i * 200000,
            "decision_interval_ms": 1000 + i * 500, "jitter_ms": 500, "risk": dict(risk),
        })
    for i, (side, position) in enumerate([("Buy", 0), ("Sell", 60)]):
        account = 420 + i
        scenario["accounts"].append({"Spot": {"account_id": account, "cash_balance": 12000, "position_qty": position}})
        add(f"pov-{side.lower()}", account, "V-BTC-SPOT", "PovExecutionTrader", {
            "side": side, "target_qty": 25, "participation_ppm": 100000 + i * 50000,
            "horizon_ms": 180000, "start_after_ms": 15000, "deadline_urgency_ms": 15000,
            "max_qty": 3, "decision_interval_ms": 1000, "jitter_ms": 0,
        })
    return spec


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:57305")
    parser.add_argument("--room", default="behavior-market")
    parser.add_argument("--seed", type=int, default=7)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--export", type=Path)
    args = parser.parse_args()
    spec = behavior_recipe(args.room, args.seed)
    if args.export:
        args.export.write_text(json.dumps(spec, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    elif args.dry_run:
        print(json.dumps(spec, ensure_ascii=False, indent=2))
    else:
        client = Client(args.base_url, bearer=os.environ.get("MARKETFORGE_AGENT_ADMIN_TOKEN"))
        result = client._request("POST", "/rooms", spec)
        print(json.dumps({"room": args.room, "bots": len(spec["agents"]), "result": result}, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
