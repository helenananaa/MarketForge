"""Bounded joint search on fit statistics, followed by untouched holdout reporting."""
import argparse
import copy
from decimal import Decimal
import hashlib
import json
import math
from pathlib import Path
import subprocess

from background_market import child_seed
from calibrate_behavior_market import fitted_recipe
from compare_behavior_reference import summarize, compare
from microstructure_market import add_perp_motives
from market_arrival_feedback import apply_feedback

ROOT = Path(__file__).resolve().parents[1]


def joint_recipe(profile, knobs, room="joint-reference", seed=7):
    limits = {"arrival_scale":(.25,2),"width_scale":(.01,2),"depth_scale":(1,32),
              "levels":(1,8),"activity":(.1,1),"market_ratio":(.1,1)}
    if set(knobs) != set(limits) or any(not isinstance(knobs[k], (int,float))
            or not math.isfinite(knobs[k]) or not low <= knobs[k] <= high for k,(low,high) in limits.items()):
        raise ValueError("joint controls outside finite bounded search space")
    if not isinstance(knobs["levels"],int) or not isinstance(knobs["depth_scale"],int):
        raise ValueError("depth and levels must be integers")
    spec, mapping = fitted_recipe(profile, room, seed)
    fit = profile["fit"]
    lot = Decimal("0.0001")
    anchor = mapping["spot_anchor_tick"]
    scale = 10
    quantities = ("max_qty", "target_qty", "inventory_cap", "inventory_target", "position_size")
    for account in spec["scenario"]["accounts"]:
        values = next(iter(account.values()))
        values["cash_balance"] *= scale
        if "position_qty" in values:
            values["position_qty"] *= scale
    for order in spec["scenario"]["seed_orders"]:
        order["NewOrder"]["qty"] *= scale
    for agent in spec["agents"]:
        config = agent["Plugin"]["config"]
        for name in quantities:
            if name in config:
                config[name] *= scale
    add_perp_motives(spec, seed)
    for account in spec["scenario"]["accounts"]:
        data = next(iter(account.values()))
        if 500 <= data["account_id"] < 505:
            data["cash_balance"] *= anchor // 100 * scale
    for agent in spec["agents"][-5:]:
        bot = agent["Plugin"]
        for name in quantities:
            if name in bot["config"]:
                bot["config"][name] *= scale
        bot["config"].update(fallback_price_tick=anchor,
            max_slippage_ticks=max(2, round(fit["perp"]["absolute_return_ppm_1s"]["p95"] or 1)))
        if bot["plugin_id"] == "LeveragedTrendTrader":
            bot["config"]["signal_threshold_ticks"] = max(1, round(fit["perp"]["absolute_return_ppm_1s"]["p50"] or 1))
    # Complete ownership stays explicit even when increasing the number of
    # independent noise traders. Every account has finite initial capital.
    existing = {leg: [] for leg in ("spot", "perp")}
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        if bot["plugin_id"] == "AdaptiveNoiseTrader":
            existing["spot" if bot["participant"]["instrument_id"].endswith("SPOT") else "perp"].append(agent)
    for leg in existing:
        count = max(len(existing[leg]), min(64, math.ceil(
            fit[leg]["aggregate_trades_per_second"] * knobs["arrival_scale"]
            / (knobs["activity"] * knobs["market_ratio"] * .75))))
        for index in range(len(existing[leg]), count):
            agent = copy.deepcopy(existing[leg][index % len(existing[leg])])
            bot = agent["Plugin"]
            name = f"joint-{leg}-flow-{index}"
            account = (1000 if leg == "spot" else 2000) + index
            bot["participant"].update(participant_id=name, account_id=account)
            bot["seed"] = max(1, child_seed(seed, name) & (2**53 - 1))
            capital = 25000 * (anchor // 100) * scale
            spec["scenario"]["accounts"].append({"Spot" if leg == "spot" else "Basic": {
                "account_id": account, "cash_balance": capital,
                **({"position_qty": capital // anchor} if leg == "spot" else {})}})
            spec["agents"].append(agent)
            existing[leg].append(agent)
    accounts = {next(iter(a.values()))["account_id"]: next(iter(a.values())) for a in spec["scenario"]["accounts"]}
    widths = {leg: max(1, round((fit[leg]["return_std_ppm_1s"] or 1) * knobs["width_scale"])) for leg in existing}
    median = {leg: max(1, round(Decimal(str(fit[leg]["aggregate_qty"]["p50"])) / lot)) for leg in existing}
    basis = round(fit["pair"]["last_trade_basis_ppm"]["p50"] or 0)
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        config = bot["config"]
        leg = "spot" if bot["participant"]["instrument_id"].endswith("SPOT") else "perp"
        account = accounts[bot["participant"]["account_id"]]
        if bot["plugin_id"] == "AdaptiveNoiseTrader":
            if leg == "spot":
                account["position_qty"] = account["cash_balance"] // anchor
            else:
                account["cash_balance"] *= 4
            config.update(decision_interval_ms=1000, jitter_ms=0, activity_ppm=round(knobs["activity"] * 1000000),
                market_order_ratio_ppm=round(knobs["market_ratio"] * 1000000), max_qty=min(10000, median[leg] * 2),
                price_radius_ticks=max(widths[leg] * 8, round(fit[leg]["absolute_return_ppm_1s"]["p95"] or 1)),
                inventory_cap=min(1000000, max(10000, account.get("position_qty", median[leg] * 100) * 4)))
        if bot["plugin_id"] == "DynamicMarketMaker":
            size = median[leg] * knobs["depth_scale"]
            cap = min(1000000, size * knobs["levels"] * 12)
            account["cash_balance"] *= 4
            if leg == "spot":
                account["position_qty"] = cap // 2
            config.update(max_qty=size, inventory_cap=cap, inventory_target=account.get("position_qty", 0),
                half_spread_ticks=widths[leg], level_spacing_ticks=max(1, widths[leg] // 2), levels=knobs["levels"],
                inventory_skew_ticks=widths[leg] * 2, volatility_spread_multiplier=0,
                withdraw_volatility_ticks=max(1000, round((fit[leg]["absolute_return_ppm_1s"]["p99"] or 1) * 8)),
                book_pressure_ticks=max(1, widths[leg] // 3), book_pressure_levels=3,
                index_basis_ticks=basis if leg == "perp" else 0,
                toxic_flow_threshold_ppm=900000, toxic_flow_min_qty=max(1, size * 2),
                toxic_cooldown_ms=3000, recovery_ramp_ms=4000, requote_threshold_ticks=max(1, widths[leg] // 3),
                decision_interval_ms=1000, jitter_ms=0, order_ttl_ms=6000)
        if config.get("risk") and bot["plugin_id"] not in ("FundingRateTrader", "LeveragedTrendTrader"):
            config["risk"]["max_volatility_ticks"] = max(1000, round((fit[leg]["absolute_return_ppm_1s"]["p99"] or 1) * 8))
        if bot["plugin_id"] == "ValueTrader":
            index = int(bot["participant"]["participant_id"].rsplit("-", 1)[1])
            config["fair_price_tick"] = anchor + round((index - 1.5) * (fit[leg]["absolute_return_ppm_1s"]["p50"] or 1))
            config["edge_ticks"] = widths[leg]
        if bot["plugin_id"] in ("ExecutionTrader", "PovExecutionTrader", "BasisArbitrageTrader"):
            config["max_slippage_ticks"] = max(config.get("max_slippage_ticks", 1), widths[leg] * 8)
    for order in spec["scenario"]["seed_orders"]:
        data = order["NewOrder"]
        data["kind"]["Limit"]["price_tick"] = anchor + (widths["spot"] if data["side"] == "Sell" else -widths["spot"])
    mapping.update(quantity_lot_btc=str(lot), cash_unit_usdt=str(Decimal(mapping["price_tick_usdt"]) * lot),
        synthetic_quote_half_spread_ticks=widths, joint_knobs=knobs, participants=len(spec["agents"]),
        initial_capital_is_finite=True)
    mapping["assumptions"] += ["arrival scaling, quote width, liquidity depth and participant count are jointly searched on fit statistics",
        "account capital and initial inventory are explicit finite scenario allocations; no replenishment during simulation",
        "perpetual venue leverage cap is 5; individual motive traders target 1, 2 or 5 times equity"]
    return spec, mapping


def fit_score(summary, fit):
    score = 0.0
    for leg in ("spot", "perp"):
        for name, weight in (("aggregate_trades_per_second", 1.0), ("return_std_ppm_1s", 1.0)):
            a, b = summary[leg][name], fit[leg][name]
            score += weight * (abs(math.log(max(a or 0, 1e-9) / max(b or 0, 1e-9))) if a is not None else 10)
        score += .5 * abs(math.log(max(summary[leg]["aggregate_qty"]["p50"] or 0, 1e-9) / max(fit[leg]["aggregate_qty"]["p50"] or 0, 1e-9)))
        score += abs(summary[leg]["active_seconds"] / summary[leg]["seconds"] - fit[leg]["active_seconds"] / fit[leg]["seconds"])
        a,b = summary[leg]["taker_side_persistence"],fit[leg]["taker_side_persistence"]
        score += abs((.5 if a is None else a) - (.5 if b is None else b))
    return score


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--reference", type=Path, default=ROOT / ".local/market-calibration/reference.json")
    parser.add_argument("--output", type=Path, default=ROOT / ".local/joint-market-calibration")
    parser.add_argument("--example", type=Path, default=ROOT / "target/release/examples/behavior_market.exe")
    parser.add_argument("--fit-steps", type=int, default=300)
    parser.add_argument("--evaluation-steps", type=int, default=600)
    parser.add_argument("--reuse-existing", action="store_true", help="reuse matching receipts from a completed report with unchanged reference and executable")
    parser.add_argument("--feedback", action="store_true", help="fit-only stochastic arrivals and replenishment; search independently from the periodic baseline")
    args = parser.parse_args()
    if not 120 <= args.fit_steps <= 1800 or not 120 <= args.evaluation_steps <= 1800:
        parser.error("fit and evaluation runs must be within 120..1800 simulation seconds")
    profile = json.loads(args.reference.read_text(encoding="utf-8"))
    for source in profile["sources"]:
        if hashlib.sha256(Path(source["path"]).read_bytes()).hexdigest() != source["sha256"]:
            raise ValueError("reference source changed")
    args.output.mkdir(parents=True, exist_ok=True)
    reference_hash = hashlib.sha256(args.reference.read_bytes()).hexdigest()
    example_hash = hashlib.sha256(args.example.read_bytes()).hexdigest()
    known_runs = {}
    prior_path = args.output / "report.json"
    if args.reuse_existing and prior_path.is_file():
        prior = json.loads(prior_path.read_text(encoding="utf-8"))
        if prior.get("reference_profile_sha256") == reference_hash and prior.get("example_sha256") == example_hash:
            for item in prior["candidates"]:
                for result in item["runs"]:
                    known_runs[(f"candidate-{item['candidate']}",result["seed"])] = result
            for label, results in prior["evaluation"].items():
                for result in results:
                    known_runs[(label,result["seed"])] = result
    def run(label, spec, mapping, seed, steps):
        if args.feedback and label != "baseline":
            apply_feedback(spec,profile["fit"])
        directory = args.output / label
        directory.mkdir(exist_ok=True)
        recipe_path = directory / "recipe.json"
        receipt_path = directory / f"simulation-{seed}.json"
        known = known_runs.get((label,seed))
        if known and known["steps"] == steps and recipe_path.is_file() and receipt_path.is_file():
            if json.loads(recipe_path.read_text(encoding="utf-8")) == spec:
                receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
                if receipt["seed"] == seed and receipt["steps"] == steps and receipt["runner"] == "continuous":
                    summary = summarize(receipt,mapping,require_order_ids=True)
                    if summary == known["summary"]:
                        return {**known,"fit_score":fit_score(summary,profile["fit"]),"reused":True,
                                "receipt_sha256":hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
                                "recipe_sha256":hashlib.sha256(recipe_path.read_bytes()).hexdigest()}
        recipe_path.write_text(json.dumps(spec, ensure_ascii=False, indent=2), encoding="utf-8")
        with receipt_path.open("w", encoding="utf-8") as output:
            subprocess.run([str(args.example.resolve()), str(seed), str(recipe_path.resolve()), str(steps), "continuous"],
                stdout=output, check=True, timeout=300)
        receipt = json.loads(receipt_path.read_text(encoding="utf-8"))
        summary = summarize(receipt, mapping, require_order_ids=True)
        return {"seed": seed, "steps": steps, "participants": receipt["bots"], "summary": summary,
                "fit_score": fit_score(summary, profile["fit"]), "trades": receipt["trades"],
                "receipt_sha256":hashlib.sha256(receipt_path.read_bytes()).hexdigest(),
                "recipe_sha256":hashlib.sha256(recipe_path.read_bytes()).hexdigest()}
    candidates = [dict(arrival_scale=arrival, width_scale=width, depth_scale=depth, levels=4,
                       activity=.8, market_ratio=.9)
                  for arrival, width, depth in ((.8,.08,8),(1,.08,8),(1.2,.08,8),(.8,.2,8),
                                               (1,.2,8),(1.2,.2,8),(1,.5,8),(1,.2,16))]
    candidates += [dict(arrival_scale=.8,width_scale=.2,depth_scale=8,levels=4,activity=activity,market_ratio=ratio)
                   for activity,ratio in ((.6,.8),(1,.7))]
    receipts = []
    for index, knobs in enumerate(candidates):
        spec, mapping = joint_recipe(profile, knobs)
        result = run(f"candidate-{index}", spec, mapping, 7, args.fit_steps)
        receipts.append({"candidate": index, "knobs": knobs, "runs": [result]})
        print(json.dumps({"candidate": index, "fit_score": result["fit_score"], "trades": result["trades"]}), flush=True)
    finalists = sorted(receipts, key=lambda item: item["runs"][0]["fit_score"])[:3]
    for item in finalists:
        spec, mapping = joint_recipe(profile, item["knobs"])
        item["runs"].append(run(f"candidate-{item['candidate']}", spec, mapping, 19, args.fit_steps))
        item["selection_score"] = sum(r["fit_score"] for r in item["runs"]) / len(item["runs"])
    selected = min(finalists, key=lambda item: item["selection_score"])
    spec, mapping = joint_recipe(profile, selected["knobs"])
    if args.feedback:apply_feedback(spec,profile["fit"])
    baseline, baseline_mapping = fitted_recipe(profile)
    evaluation = {"baseline": [], "selected": []}
    for seed in (7,19,41):
        for label, recipe, units in (("baseline",baseline,baseline_mapping),("selected",spec,mapping)):
            result = run(label,recipe,units,seed,args.evaluation_steps)
            # Holdout is accessed only after the selected recipe is frozen.
            result["holdout_residuals"] = compare(result["summary"], profile["holdout"])
            evaluation[label].append(result)
    for name, value in (("recipe.json",spec),("mapping.json",mapping)):
        (args.output/name).write_text(json.dumps(value, ensure_ascii=False, indent=2)+"\n",encoding="utf-8")
    report = {"reference_day_utc": profile["day_utc"], "used_holdout_to_fit":False,
        "arrival_feedback":args.feedback,
        "reference_profile_sha256":reference_hash,
        "selected_recipe_sha256":hashlib.sha256((args.output/"recipe.json").read_bytes()).hexdigest(),
        "aggregation":"spot contiguous same order/time/price; perp contiguous same 100ms bucket/price/side; simulation clock remains 1s",
        "example_sha256":example_hash,
        "candidates":receipts,"selected_candidate":selected["candidate"],"evaluation":evaluation,
        "qualification":"bounded fit-only joint search and untouched holdout residuals; no real-market qualification"}
    (args.output/"report.json").write_text(json.dumps(report,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    print(json.dumps({"selected":selected["candidate"],"participants":len(spec["agents"]),"passed_pipeline":True}),flush=True)


if __name__ == "__main__":
    main()
