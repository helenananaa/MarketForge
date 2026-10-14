"""Mixed or pure-Pine live load; every tier retains its full receipt."""
import argparse
import copy
import json
import math
import os
from collections import Counter
from pathlib import Path
import sys
import threading
import time
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[1]
HOST_EXPERIMENTS = {
    "MARKETFORGE_PINE_SOURCE_WAVE": ("0", "1"),
}


def host_experiment(value):
    name, separator, setting = value.partition("=")
    if not separator or name not in HOST_EXPERIMENTS or setting not in HOST_EXPERIMENTS[name]:
        raise argparse.ArgumentTypeError("invalid or unsupported host experiment NAME=VALUE")
    return name, setting
sys.path.insert(0, str(ROOT / "python"))
import benchmark_market_load as load
from background_market import child_seed
from process_tree_metrics import ProcessTreeMetrics
from bot_runtime_receipts import audit_receipts, runtime_delta, runtime_metrics


def mixed_recipe(count, room, seed, staggered=False, template=None):
    if not 100 <= count <= 3000: raise ValueError("use 100..3000 total participants")
    native = count - count // 10 * 2
    # The frozen native driver has a 1000 participant bound; extra noise is
    # allocated explicitly below rather than changing that driver's limits.
    spec = ORIGINAL(min(native, 1000), room, seed, staggered, template)
    # Probe a separate empty book: a latency probe must not become a strategy's
    # best bid/ask when liquidity disappears on the traded markets.
    probe = copy.deepcopy(spec["scenario"]["market"])
    probe["Spot"]["instrument"].update(instrument_id="LOAD-PROBE-SPOT", symbol="LOAD-PROBE-SPOT")
    spec["scenario"]["extra_markets"].append(probe)
    noise = next(a for a in spec["agents"] if a["Plugin"]["plugin_id"] == "AdaptiveNoiseTrader" and a["Plugin"]["participant"]["instrument_id"] == "V-BTC-SPOT")
    account = next(a for a in spec["scenario"]["accounts"] if next(iter(a.values()))["account_id"] == noise["Plugin"]["participant"]["account_id"])
    for i in range(native - min(native, 1000)):
        agent = copy.deepcopy(noise);allocation = copy.deepcopy(account);identifier = 20000 + i
        agent["Plugin"]["participant"].update(account_id=identifier, participant_id=f"extra-noise-{i}")
        agent["Plugin"]["seed"] = child_seed(seed, f"extra-noise-{i}") & (2**53-1) or 1
        next(iter(allocation.values()))["account_id"] = identifier
        spec["agents"].append(agent);spec["scenario"]["accounts"].append(allocation)
    for language in ("pine", "python"):
        for i in range(count // 10):
            identifier = (50000 if language == "pine" else 60000) + i
            name = f"{language}-{i}"
            spec["scenario"]["accounts"].append({"Spot":{"account_id":identifier,"cash_balance":10000,"position_qty":0}})
            config = {"bar_interval_ms":[10000,15000,30000][i % 3]}
            if language == "pine": config.update(script=["ma.pine","rsi.pine","breakout.pine"][i % 3], history_limit=512)
            else: config.update(fast=3 + i % 3, slow=8 + i % 5)
            spec["agents"].append({"Plugin":{"participant":{"participant_id":name,"room_id":room,"account_id":identifier,
                "kind":"RuleAgent","instrument_id":"V-BTC-SPOT"}, "plugin_id":"pine.strategy" if language == "pine" else "python.sma",
                "plugin_version":"2.0.0" if language == "pine" else "1.0.0", "state_version":1,"config_version":1,
                "seed":child_seed(seed,name)&(2**53-1) or 1,"config":config}})
    assert len(spec["agents"]) == count
    return spec


def pine_config(index, seed, profile):
    """Only inputs and actual candle periods vary; no trading delays are injected."""
    script = ["ma.pine", "rsi.pine", "breakout.pine"][index % 3]
    if profile in ("short", "diverse-short"):
        interval = [10000, 15000, 30000][index % 3]
    elif profile == "mixed-horizon":
        interval = [60000] * 6 + [300000] * 3 + [3600000]
        interval = interval[index % 10]
    elif profile == "slow-horizon":
        interval = [300000] * 5 + [3600000] * 5
        interval = interval[index % 10]
    else:
        raise ValueError(f"unknown Pine workload profile: {profile}")
    config = dict(script=script, bar_interval_ms=interval, history_limit=512)
    if profile != "short":
        noise = child_seed(seed, f"pine-inputs-{index}")
        if script == "ma.pine":
            fast = 2 + noise % 12
            inputs = {"Fast":fast, "Slow":fast + 4 + (noise // 12) % 40}
        elif script == "rsi.pine":
            inputs = {"Length":3 + noise % 25, "Low":20 + (noise // 25) % 25,
                      "High":55 + (noise // 625) % 25}
        else:
            inputs = {"Length":3 + noise % 40}
        config["inputs"] = {**inputs, "Quantity":1 + (noise // 15625) % 2}
    return config


def workload_summary(spec):
    configs = [a["Plugin"]["config"] for a in spec["agents"]
               if a["Plugin"]["plugin_id"] == "pine.strategy"]
    groups = Counter(c["bar_interval_ms"] for c in configs)
    return {"period_instances":{str(k):v for k,v in sorted(groups.items())},
            "ideal_bar_updates_per_sim_second":sum(v * 1000 / k for k,v in groups.items()),
            "distinct_inputs":len({json.dumps(c.get("inputs", {}), sort_keys=True) for c in configs}),
            "scope":"ideal steady bar rate assumes nonempty bars; excludes account wakes, host polling, cold starts and rebuilds",
            "timing":"common exchange candle boundaries retained; parameter diversity does not inject execution jitter"}


def diagnostic_metrics(path):
    if not path.is_file():
        return {}
    values = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("marketforge_bench_"):
            name, value = line.split()
            values[name.removeprefix("marketforge_bench_")] = int(value)
    stages = {}
    for key, count in values.items():
        if not key.endswith("_count_total"):
            continue
        name = key.removesuffix("_count_total")
        stages[name] = {"count":count, "total_wall_ms":values[f"{name}_nanos_total"] / 1e6,
                       "mean_wall_ms":values[f"{name}_nanos_total"] / count / 1e6 if count else None,
                       "max_wall_ms":values[f"{name}_max_nanos"] / 1e6,
                       "p95_upper_wall_ms":values[f"{name}_p95_upper_nanos"] / 1e6
                           if f"{name}_p95_upper_nanos" in values and count else None}
    return {"stages":stages, "scope":"whole run including startup/warmup; nested cumulative wall times include waits and must not be summed",
            "p95":"power-of-two nanosecond histogram upper bound, not exact sample P95",
            "worker_roundtrip":"pipe send, Python decode/decision/encode, output receive; not interpreter-only CPU time",
            "worker_acquire":"affinity-slot wait plus cold process startup when needed"} if stages else {}


def pine_recipe(count, room, seed, staggered=False, template=None, *, profile="short"):
    """Count denotes actual Pine instances; 38 fixed native liquidity bots remain."""
    if not 100 <= count <= 3000: raise ValueError("use 100..3000 Pine instances")
    spec = ORIGINAL(38, room, seed, staggered, template)
    # Fund maker depth for the actual participant count at initialization.
    # Otherwise Pine demand empties the fixed 38-bot book and a quiet tail
    # misleadingly appears to have caught up with every required candle.
    allocations = {next(iter(a.values()))["account_id"]:next(iter(a.values()))
                   for a in spec["scenario"]["accounts"]}
    scale = (count + 38) / 38
    for agent in spec["agents"]:
        bot = agent["Plugin"]
        if bot["plugin_id"] == "DynamicMarketMaker":
            for name in ("max_qty", "inventory_cap", "inventory_target", "toxic_flow_min_qty"):
                if name in bot["config"]: bot["config"][name] = math.ceil(bot["config"][name] * scale)
            account = allocations[bot["participant"]["account_id"]]
            for name in ("cash_balance", "position_qty"):
                if name in account: account[name] = math.ceil(account[name] * scale)
    probe = copy.deepcopy(spec["scenario"]["market"])
    probe["Spot"]["instrument"].update(instrument_id="LOAD-PROBE-SPOT", symbol="LOAD-PROBE-SPOT")
    spec["scenario"]["extra_markets"].append(probe)
    for i in range(count):
        identifier = 50000 + i
        name = f"pine-{i}"
        spec["scenario"]["accounts"].append({"Spot":{"account_id":identifier,"cash_balance":10000,"position_qty":0}})
        spec["agents"].append({"Plugin":{"participant":{"participant_id":name,"room_id":room,"account_id":identifier,
            "kind":"RuleAgent","instrument_id":"V-BTC-SPOT"}, "plugin_id":"pine.strategy", "plugin_version":"2.0.0",
            "state_version":1,"config_version":1,"seed":child_seed(seed,name)&(2**53-1) or 1,
            "config":pine_config(i, seed, profile)}})
    assert len(spec["agents"]) == count + 38
    return spec


ORIGINAL = load.load_recipe


def input_price(profile, market_time_ms):
    if profile == "flat":
        return 100
    if profile == "cycle":
        phase = (market_time_ms // 5_000) % 80
        return 80 + min(phase, 80-phase)
    raise ValueError(f"unknown input profile: {profile}")


def sustained_coverage_required(args):
    # A ten-second correctness smoke is not a sixty-second workload qualification.
    return args.input_profile == "cycle" and args.require_bot_receipts and args.seconds >= 60


def trade_price_summary(trades):
    """Prices are from matched trade receipts, never the submitted limit."""
    prices = [trade["price"] for trade in trades]
    changes = [b-a for a,b in zip(prices, prices[1:])]
    return {"trade_count":len(prices), "min_tick":min(prices) if prices else None,
        "max_tick":max(prices) if prices else None, "first_tick":prices[0] if prices else None,
        "last_tick":prices[-1] if prices else None, "distinct_ticks":len(set(prices)),
        "upward_changes":sum(change > 0 for change in changes),
        "downward_changes":sum(change < 0 for change in changes),
        "unchanged":sum(change == 0 for change in changes)}


def feed_execution_summary(feeds):
    trades, limits = {}, []
    orders, matched_orders = 0, 0
    for feed in feeds:
        for iteration in feed["receipts"]:
            for entry in iteration["orders"]:
                response = entry["response"]
                orders += 1
                limit = response.get("action", {}).get("PlaceLimit", {}).get("price_tick")
                if limit is not None: limits.append(limit)
                matched = False
                for event in response["events"]:
                    if event["type"] != "TradePrinted": continue
                    matched = True
                    key = (response.get("instrument_id", "V-BTC-SPOT"), event["trade_id"])
                    trades[key] = {"price":event["price_tick"], "qty":event["qty"],
                        "market_time_ms":response["market_time_ms"], "trade_id":event["trade_id"]}
                matched_orders += matched
    ordered = sorted(trades.values(), key=lambda trade:(trade["market_time_ms"], trade["trade_id"]))
    return {"orders":orders, "orders_with_fills":matched_orders,
        "orders_without_fills":orders-matched_orders, "unique_trades":len(ordered),
        "matched_qty":sum(trade["qty"] for trade in ordered),
        "requested_limit_ticks":{"min":min(limits) if limits else None, "max":max(limits) if limits else None},
        "actual_trade_prices":trade_price_summary(ordered),
        "scope":"actual TradePrinted receipts from sustained gateway feed orders; whole feed including warmup and shutdown; requested limits are separate and do not guarantee the traded price"}


def pine_ledger_fill_summary(scheduler):
    """Final own-fill ledgers prove fills; accepted IOC receipts alone do not."""
    sources, trades, errors, missing = {}, {}, [], []
    for agent in scheduler["agents"]:
        config = agent["template"]["Plugin"]
        if config["plugin_id"] != "pine.strategy": continue
        source = config["config"]["script"]
        participant = config["participant"]["participant_id"]
        group = sources.setdefault(source, {"participants":0, "participants_with_fills":0,
            "fill_receipts":0, "participant_filled_qty":0, "buy_fill_receipts":0, "sell_fill_receipts":0})
        group["participants"] += 1
        data = agent["kind_state"].get("Plugin", {}).get("data")
        ledgers = data.get("ledgers") if isinstance(data, dict) else None
        if not isinstance(ledgers, dict) or not ledgers:
            missing.append(participant)
            continue
        count = 0
        for instrument, ledger in ledgers.items():
            fills = ledger.get("fills") if isinstance(ledger, dict) else None
            if not isinstance(fills, list):
                errors.append(f"{participant}: missing fill list for {instrument}")
                continue
            previous_id = -1
            for fill in fills:
                if (not isinstance(fill, dict) or any(type(fill.get(key)) is not int
                        or fill[key] < (1 if key in ("price", "qty") else 0)
                        for key in ("id", "time", "price", "qty"))
                        or type(fill.get("buy")) is not bool or fill["id"] <= previous_id):
                    errors.append(f"{participant}: invalid or nonmonotonic fill for {instrument}")
                    continue
                previous_id = fill["id"]
                count += 1
                group["fill_receipts"] += 1
                group["participant_filled_qty"] += fill["qty"]
                group["buy_fill_receipts" if fill["buy"] else "sell_fill_receipts"] += 1
                key = (instrument, fill["id"])
                trade = {"price":fill["price"], "qty":fill["qty"],
                    "market_time_ms":fill["time"], "trade_id":fill["id"]}
                if key in trades and trades[key] != trade:
                    errors.append(f"conflicting Pine fill receipts for {key}")
                trades[key] = trade
        group["participants_with_fills"] += count > 0
    ordered = sorted(trades.values(), key=lambda trade:(trade["market_time_ms"], trade["trade_id"]))
    return {"by_source":sources, "unique_trades":len(ordered),
        "matched_qty":sum(trade["qty"] for trade in ordered),
        "actual_trade_prices":trade_price_summary(ordered),
        "first_fill_market_time_ms":ordered[0]["market_time_ms"] if ordered else None,
        "last_fill_market_time_ms":ordered[-1]["market_time_ms"] if ordered else None,
        "missing_participants":missing, "errors":errors,
        "complete_final_ledgers":bool(sources) and not missing and not errors,
        "scope":"final paused Pine own-fill ledgers, including warmup and drain; distinct market trades deduplicated across Pine counterparties; per-source quantities count participant fills; this is not wall-clock cutoff or per-minute fill proof"}


class SustainedInput:
    """Finite funded traders create real fills through the exchange gateway."""
    def __init__(self, client, room, iteration_budget=2000, input_profile="flat"):
        self.client, self.room = client, room
        self.iteration_budget = iteration_budget
        self.input_profile = input_profile
        self.stop = threading.Event()
        self.receipts, self.errors = [], []
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def run(self):
        previous = None
        try:
            while not self.stop.is_set():
                clock = self.client.clock(self.room)["clock"]
                if clock["step"] != previous:
                    previous = clock["step"]
                    number = len(self.receipts)
                    if number >= self.iteration_budget: raise RuntimeError("finite input feed exhausted")
                    price = input_price(self.input_profile, clock["market_time_ms"])
                    responses = []
                    for side, account in (("Sell", 900001), ("Buy", 900002)):
                        result = self.client._request("POST", f"/rooms/{self.room}/instruments/V-BTC-SPOT/orders",
                            {"participant_id":f"load-feed-{side}", "account_id":account,
                             "action":{"PlaceLimit":{"side":side,"price_tick":price,"qty":1}}},
                            idempotency_key=f"feed-{number}-{side}")
                        if not result.get("accepted"): raise RuntimeError(f"input order rejected: {result}")
                        responses.append({"account":account, "response":result})
                    for entry in responses:
                        for event in entry["response"]["events"]:
                            if event["type"] == "OrderRested":
                                result = self.client.submit_action(self.room, "V-BTC-SPOT", entry["account"],
                                    {"Cancel":{"order_id":event["order_id"]}},
                                    f'feed-{number}-cancel-{event["order_id"]}')
                                if not result.get("accepted"): raise RuntimeError(f"input cleanup rejected: {result}")
                    self.receipts.append({"clock":clock, "requested_price_tick":price, "orders":responses})
                self.stop.wait(.05)
        except Exception as error:
            self.errors.append(repr(error))

    def finish(self):
        self.stop.set()
        self.thread.join(timeout=10)
        if self.thread.is_alive(): raise RuntimeError("input feed did not stop")
        return {"scope":"real gateway limit orders; two finite funded accounts; no injected candles; limit prices do not guarantee execution prices",
                "input_profile":self.input_profile,
                "iterations":len(self.receipts), "iteration_budget":self.iteration_budget,
                "errors":self.errors, "receipts":self.receipts}


def duration_summary(path, tree, warmup, seconds, target_rate):
    """Retain the time curve: a fast early window cannot qualify a stalled tail."""
    samples = json.loads(path.read_text(encoding="utf-8"))["clock"]
    measured = [row for row in samples if warmup <= row["elapsed"] <= warmup + seconds]
    windows = []
    boundaries = list(range(0, math.ceil(seconds), 60))
    if len(boundaries) > 1 and seconds - boundaries[-1] < 5:
        boundaries.pop()  # A sub-poll-duration remainder is not a separate window.
    for index, start in enumerate(boundaries):
        end = boundaries[index+1] if index+1 < len(boundaries) else seconds
        rows = [row for row in measured if warmup + start <= row["elapsed"] <= warmup + end]
        span = rows[-1]["elapsed"] - rows[0]["elapsed"] if len(rows) > 1 else 0
        rate = (rows[-1]["step"] - rows[0]["step"]) / span if span else None
        max_gap = max((b["elapsed"]-a["elapsed"] for a,b in zip(rows, rows[1:])), default=None)
        windows.append({"from_second":start, "to_second":end, "sample_span_seconds":span,
                        "steps_per_second":rate,
                        "max_sample_gap_seconds":max_gap,
                        "covered":len(rows) > 1 and span >= end-start-.75 and max_gap <= 2,
                        "at_least_95_percent_target":rate is not None and rate >= .95 * target_rate})
    rss = tree.get("samples", []) if tree else []
    measurement = (tree or {}).get("measurement", {})
    # Process sampler clock has a different origin; use its recorded bounds.
    bounds = measurement.get("elapsed_bounds")
    rss = [row for row in rss if bounds and bounds[0] <= row["elapsed"] <= bounds[1]]
    resource_span = rss[-1]["elapsed"]-rss[0]["elapsed"] if len(rss) > 1 else 0
    resource_gap = max((b["elapsed"]-a["elapsed"] for a,b in zip(rss, rss[1:])), default=None)
    resource_covered = bool(bounds and len(rss) > 1 and resource_span >= bounds[1]-bounds[0]-.75
                            and resource_gap <= 2 and not tree.get("errors"))
    stalled = 0
    if measured:
        unchanged_since = measured[0]["elapsed"]
        previous_step = measured[0]["step"]
        for row in measured[1:]:
            if row["step"] != previous_step:
                unchanged_since = row["elapsed"]
                previous_step = row["step"]
            else:
                stalled = max(stalled, row["elapsed"]-unchanged_since)
    slope = None
    if len(rss) > 1:
        mean_x = sum(row["elapsed"] for row in rss) / len(rss)
        mean_y = sum(row["rss_bytes"] for row in rss) / len(rss)
        denominator = sum((row["elapsed"]-mean_x)**2 for row in rss)
        slope = sum((row["elapsed"]-mean_x)*(row["rss_bytes"]-mean_y) for row in rss) / denominator * 60 / 1024**2 if denominator else None
    return {"windows":windows, "rss_slope_mib_per_minute":slope,
            "process_sampling_complete":resource_covered,
            "process_max_sample_gap_seconds":resource_gap,
            "longest_observed_no_progress_seconds":stalled,
            "full_requested_duration_observed":bool(windows) and all(row["covered"] for row in windows),
            "sustained_target":bool(windows) and all(row["covered"] and row["at_least_95_percent_target"] for row in windows),
            "scope":"60-second observed clock windows; RSS least-squares slope includes retained durable journal growth"}


def prewarm_market(client, room, spec, longest_bars):
    configs = [a["Plugin"]["config"] for a in spec["agents"] if a["Plugin"]["plugin_id"] == "pine.strategy"]
    periods = sorted({c["bar_interval_ms"] for c in configs})
    stride = math.gcd(*periods)
    if stride % 1000: raise ValueError("prewarm periods must be whole simulation steps")
    iterations = longest_bars * periods[-1] // stride
    if iterations > 512: raise ValueError("bounded gateway history prewarming exceeded")
    receipts = []
    begun = time.perf_counter()
    for number in range(iterations):
        # Rising/falling segments warm all three bundled strategies.
        phase = number % 40
        price = 90 + (phase if phase < 20 else 40 - phase)
        responses = []
        for side, account in (("Sell", 900001), ("Buy", 900002)):
            response = client._request("POST", f"/rooms/{room}/instruments/V-BTC-SPOT/orders",
                {"participant_id":f"prewarm-{side}", "account_id":account,
                 "action":{"PlaceLimit":{"side":side,"price_tick":price,"qty":1}}},
                idempotency_key=f"prewarm-{number}-{side}")
            if not response.get("accepted"): raise RuntimeError(f"prewarm order rejected: {response}")
            responses.append({"account":account, "response":response})
        for entry in responses:
            for event in entry["response"]["events"]:
                if event["type"] == "OrderRested":
                    response = client.submit_action(room, "V-BTC-SPOT", entry["account"],
                        {"Cancel":{"order_id":event["order_id"]}}, f'prewarm-{number}-cancel-{event["order_id"]}')
                    if not response.get("accepted"): raise RuntimeError(f"prewarm cleanup rejected: {response}")
        client.advance_clock(room, stride // 1000)
        receipts.append({"price_tick":price, "orders":responses})
    history = {}
    for period in periods:
        page = client.candles(room, "V-BTC-SPOT", interval_ms=period, limit=2000)
        history[str(period)] = sum(bar["is_final"] for bar in page["candles"])
        if history[str(period)] < longest_bars: raise RuntimeError("prewarm did not produce required real closed bars")
    return {"scope":"real gateway fills before bot startup; computation/account state initializes afterwards; not historic Pine trading or restart qualification",
            "iterations":iterations, "elapsed_seconds":time.perf_counter()-begun,
            "closed_bars_by_period":history, "receipts":receipts}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--host-experiment", type=host_experiment, action="append", default=[],
                        help="explicit isolated server experiment NAME=VALUE; inherited MARKETFORGE variables are stripped")
    parser.add_argument("--pine-in-process", action="store_true", help="enable the optional qualified Rust Pine backend")
    parser.add_argument("--pine-vm-checkpoint", action="store_true", help="opt in to compact schema-3 checkpoints; requires --pine-in-process")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--counts", type=int, nargs="+", default=[100,500,1000,3000])
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--long-run", action="store_true", help="explicitly permit measurements up to 3600 seconds with a larger finite funded input budget; requires --require-bot-receipts")
    parser.add_argument("--max-memory-mib", type=float, default=4096, help="explicit server memory guard; raising it changes the acceptance contract")
    parser.add_argument("--interval-ms", type=int, default=25)
    parser.add_argument("--plugin-dir", type=Path, default=ROOT / "bot-plugins")
    parser.add_argument("--population", choices=("mixed", "pine"), default="mixed",
                        help="pine counts actual Pine bots plus 38 fixed native liquidity providers")
    parser.add_argument("--sustained-input", action="store_true",
                        help="pure Pine only: funded gateway traders keep nonempty candles arriving")
    parser.add_argument("--input-profile", choices=("flat", "cycle"), default="flat",
                        help="sustained gateway limits: flat=100, cycle=80..120..80 every 400 simulation seconds; actual trades are audited separately")
    parser.add_argument("--pine-profile", choices=("short", "diverse-short", "mixed-horizon", "slow-horizon"), default="short",
                        help="mixed-horizon: 60%% 1m, 30%% 5m, 10%% 1h; slow-horizon: 50%% 5m, 50%% 1h; assumed workloads, not captured production traffic")
    parser.add_argument("--start-step", type=int, default=0,
                        help="advance empty simulation before starting bots; 3590 exercises common hour close shortly after startup, not long-history acceptance")
    parser.add_argument("--require-diagnostics", action="store_true",
                        help="fail qualification unless server was built with bench-execution-timing")
    parser.add_argument("--trace-graceful-shutdown", action="store_true",
                        help="candidate capability: require drained and flushed trace acknowledgement at shutdown")
    parser.add_argument("--trace-shutdown-timeout", type=float, default=15,
                        help="cleanup-only graceful trace deadline in seconds; timeout invalidates the case")
    parser.add_argument("--bot-trace", action="store_true",
                        help="retain per-decision/action durable receipts; adds diagnostic trace overhead")
    parser.add_argument("--require-bot-receipts", action="store_true",
                        help="require complete durable outcomes and exercised accepted bar-close latency; implies --bot-trace")
    parser.add_argument("--max-bar-close-to-admission-ms", type=float, default=250,
                        help="accepted action P95 from actual bar-close publication to durable admission")
    parser.add_argument("--event-prefilter", action="store_true",
                        help="opt in to the experimental host event prefilter; default disabled")
    parser.add_argument("--prewarm-bars", type=int, default=0,
                        help="create real gateway history before starting bots: this many bars at the longest configured period; no injected candles")
    parser.add_argument("--warmup", type=float, default=5,
                        help="exclude this many wall seconds after bot startup from clock/probe measurements (0..30)")
    parser.add_argument("--export-only", action="store_true", help="freeze recipes and workload summaries without launching a server")
    args = parser.parse_args()
    if args.trace_graceful_shutdown and not args.require_bot_receipts:
        parser.error("--trace-graceful-shutdown requires --require-bot-receipts")
    if not 1 <= args.trace_shutdown_timeout <= 60:
        parser.error("trace shutdown timeout must be between 1 and 60 seconds")
    if args.long_run and not args.require_bot_receipts:
        parser.error("--long-run requires --require-bot-receipts for terminal outcomes and cutoff progress")
    if not math.isfinite(args.max_bar_close_to_admission_ms) or args.max_bar_close_to_admission_ms <= 0:
        parser.error("bar-close latency threshold must be finite and positive")
    if not 5 <= args.seconds <= (3600 if args.long_run else 120) or not 25 <= args.interval_ms <= 1000: parser.error("bounded duration/clock required; use --long-run above 120 seconds")
    if not 0 < args.max_memory_mib <= (16384 if args.long_run else 4096): parser.error("memory guard must be finite, positive and within the explicit duration mode limit")
    if args.pine_vm_checkpoint and not args.pine_in_process: parser.error("VM checkpoints require --pine-in-process")
    if args.sustained_input and args.population != "pine": parser.error("sustained input requires pure Pine")
    if args.input_profile == "cycle" and not args.sustained_input: parser.error("cycle input requires --sustained-input")
    if not 0 <= args.start_step <= 10000: parser.error("start-step must be within 0..10000")
    if not 0 <= args.prewarm_bars <= 128 or not 0 <= args.warmup <= 30: parser.error("bounded prewarm-bars/warmup required")
    if args.prewarm_bars and (args.population != "pine" or not args.sustained_input or args.start_step):
        parser.error("history prewarming requires pure Pine sustained input and start-step 0")
    if args.population != "pine" and (args.pine_profile != "short" or args.start_step):
        parser.error("Pine workload and start-step options require pure Pine")
    args.output.mkdir(parents=True, exist_ok=True)
    feed_budget = max(2000, math.ceil((args.warmup + args.seconds) * 1000 / args.interval_ms * 1.1)) if args.long_run else 2000
    # A scoped child environment installs only these trusted local packages.
    previous = os.environ.get("MARKETFORGE_BOT_PLUGIN_DIR")
    os.environ["MARKETFORGE_BOT_PLUGIN_DIR"] = str(args.plugin_dir.resolve())
    previous_prefilter = os.environ.get("MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER")
    os.environ["MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER"] = "1" if args.event_prefilter else "0"
    def recipe(*values, **kwargs):
        spec = pine_recipe(*values, **kwargs, profile=args.pine_profile) if args.population == "pine" else mixed_recipe(*values, **kwargs)
        if args.sustained_input:
            for account, position in ((900001, max(10000, feed_budget + 512)), (900002, 0)):
                spec["scenario"]["accounts"].append({"Spot":{"account_id":account,
                    "cash_balance":max(10000000, (feed_budget+512)*200),"position_qty":position}})
        if args.prewarm_bars:
            configs = [a["Plugin"]["config"] for a in spec["agents"] if a["Plugin"]["plugin_id"] == "pine.strategy"]
            span = max(c["bar_interval_ms"] for c in configs) * args.prewarm_bars
            if any(math.ceil(span / c["bar_interval_ms"]) + 16 > c["history_limit"] for c in configs):
                raise ValueError("prewarm history would exceed a configured history_limit; this case requires long-term checkpoint support")
        return spec
    if args.export_only:
        for count in args.counts:
            spec = recipe(count, f"load-{count}", 7)
            (args.output / f"bots-{count}").mkdir(parents=True, exist_ok=True)
            load.save(args.output / f"bots-{count}/recipe.json", spec)
            load.save(args.output / f"bots-{count}/workload.json", workload_summary(spec))
        if previous is None: os.environ.pop("MARKETFORGE_BOT_PLUGIN_DIR", None)
        else: os.environ["MARKETFORGE_BOT_PLUGIN_DIR"] = previous
        return
    load.load_recipe = recipe
    # run_case strips MARKETFORGE_* variables; pass the installation through its
    # subprocess wrapper while preserving its loopback and fresh storage setup.
    popen = load.subprocess.Popen
    monitors = []
    measurement_timers = []
    active_case_output = None
    def installed_popen(*values, **kwargs):
        if kwargs.get("env", {}).get("MARKETFORGE_BIND_ADDR"):
            kwargs["env"]["MARKETFORGE_PINE_IN_PROCESS"] = "1" if args.pine_in_process else "0"
            kwargs["env"]["MARKETFORGE_PINE_VM_CHECKPOINT"] = "1" if args.pine_vm_checkpoint else "0"
            kwargs["env"]["MARKETFORGE_BOT_PLUGIN_DIR"] = str(args.plugin_dir.resolve())
            kwargs["env"].update(dict(args.host_experiment))
            load.save(active_case_output / "host-experiment-env.json", {
                name: kwargs["env"].get(name) for name in HOST_EXPERIMENTS
            })
            if args.bot_trace or args.require_bot_receipts:
                trace_path = (active_case_output / "bot-receipts.jsonl").resolve()
                if trace_path.exists():
                    raise RuntimeError("refusing to overwrite existing bot trace receipt")
                kwargs["env"]["MARKETFORGE_BOT_TRACE"] = str(trace_path)
            process = popen(*values, **kwargs)
            monitors.append(ProcessTreeMetrics(process.pid))
            return process
        return popen(*values, **kwargs)
    load.subprocess.Popen = installed_popen
    original_client = load.Client
    feeds = []
    class RequiredInputClient(original_client):
        def place(self, room, account, side, price, qty, **kwargs):
            if account == 900000 and side == "Buy" and price == 1 and qty == 1:
                return self.submit_action(room, "LOAD-PROBE-SPOT", account,
                    {"PlaceLimit":{"side":"Sell", "price_tick":101, "qty":1}}, kwargs.get("idempotency_key"))
            return super().place(room, account, side, price, qty, **kwargs)

        def cancel(self, room, account, order, **kwargs):
            if account == 900000:
                return self.submit_action(room, "LOAD-PROBE-SPOT", account, {"Cancel":{"order_id":order}}, kwargs.get("idempotency_key"))
            return super().cancel(room, account, order, **kwargs)

        def _request(self, *values, **kwargs):
            try:
                create = values[:2] == ("POST", "/rooms")
                if create and (args.start_step or args.prewarm_bars):
                    spec = copy.deepcopy(values[2])
                    spec["autostart_agents"] = False
                    values = (*values[:2], spec, *values[3:])
                result = super()._request(*values, **kwargs)
                if create:
                    room = values[2]["scenario"]["room_id"]
                    case_output = args.output / f'bots-{room.removeprefix("load-")}'
                    control = original_client(self.base_url, timeout=10)
                    if args.prewarm_bars:
                        warm = prewarm_market(control, room, values[2], args.prewarm_bars)
                        load.save(case_output / 'prewarm.json', warm)
                        periods = sorted({a['Plugin']['config']['bar_interval_ms'] for a in values[2]['agents'] if a['Plugin']['plugin_id'] == 'pine.strategy'})
                        initial_history = {str(period): control.candles(room, 'V-BTC-SPOT', interval_ms=period, limit=2000)['candles'] for period in periods}
                        load.save(case_output / 'prewarm-candles.json', initial_history)
                    if args.start_step:
                        control.advance_clock(room, args.start_step)
                    if args.start_step or args.prewarm_bars:
                        control.start_agents(room, values[2]["agents"], args.interval_ms)
                    if args.sustained_input:
                        feeds.append(SustainedInput(original_client(self.base_url, timeout=3), room, feed_budget, args.input_profile))
                    origin = time.perf_counter()
                    if monitors:
                        start = origin - monitors[-1].begun + args.warmup
                        monitors[-1].measurement_window = (start, start + args.seconds)
                    def capture_metrics(label):
                        request_started = time.perf_counter()
                        try:
                            body = load.urllib.request.urlopen(self.base_url + "/metrics", timeout=3).read().decode("utf-8")
                            (case_output / f"measurement-{label}-metrics.txt").write_text(body, encoding="utf-8")
                            load.save(case_output / f"measurement-{label}.json", {
                                "request_started_after_start":request_started-origin,
                                "elapsed_after_start":time.perf_counter()-origin})
                        except Exception as error:
                            load.save(case_output / f"measurement-{label}-error.json", {"error":repr(error)})
                    for label, delay in (("start", args.warmup), ("end", args.warmup+args.seconds)):
                        timer = threading.Timer(delay, capture_metrics, (label,))
                        timer.daemon = True
                        timer.start()
                        measurement_timers.append(timer)
                return result
            except Exception as error:
                raise RuntimeError(f"{type(error).__name__}: {error!r}") from error

        def pause_room(self, *values, **kwargs):
            for feed in feeds: feed.finish()
            return super().pause_room(*values, **kwargs)

        def room_bots(self, room):
            saved = super().room_bots(room)
            required = {}
            for agent in saved["agents"]:
                config = agent["template"]["Plugin"]
                if config["plugin_id"] not in ("pine.strategy", "python.sma"): continue
                interval = config["config"]["bar_interval_ms"]
                instrument = config["participant"]["instrument_id"]
                key = f"{instrument}:{interval}"
                if key not in required:
                    page = self.candles(room, instrument, interval_ms=interval, limit=2000)
                    bars = list(page["candles"])
                    earlier = page
                    while len(earlier["candles"]) == 2000:
                        before = earlier["candles"][0]["open_time_ms"]
                        earlier = self.candles(room, instrument, interval_ms=interval, limit=2000, before_open_time_ms=before)
                        if any(bar["open_time_ms"] >= before for bar in earlier["candles"]):
                            raise RuntimeError("required-bar pagination failed to advance")
                        if earlier["market_time_ms"] != page["market_time_ms"]:
                            raise RuntimeError("required-bar audit requires a paused market")
                        bars[:0] = earlier["candles"]
                    closed = [bar for bar in bars if bar["is_final"]]
                    required[key] = {"closed_bars":len(closed), "latest_open_ms":closed[-1]["open_time_ms"] if closed else None,
                                     "closed_bar_close_times_ms":[bar["close_time_ms"] for bar in closed],
                                     "market_time_ms":page["market_time_ms"]}
            saved["benchmark_required_inputs"] = required
            return saved
    load.Client = RequiredInputClient
    try:
        cases = []
        for count in args.counts:
            active_case_output = args.output / f"bots-{count}"
            run_args = SimpleNamespace(**vars(args), recipe_template=None, seed=7, staggered=False,
                browser_wait=0)
            try:
                report = load.run_case(run_args, count)
            except Exception as error:
                report = {"bots":count,"failure":repr(error),"qualified":False}
                load.save(args.output / f"bots-{count}/failure.json", report)
                cases.append(report)
                print(json.dumps(report), flush=True)
                continue
            finally:
                for timer in measurement_timers:
                    timer.cancel();timer.join(timeout=4)
                measurement_timers.clear()
                feed_receipts = [feed.finish() for feed in feeds]
                feeds.clear()
                if feed_receipts: load.save(args.output / f"bots-{count}/input-feed.json", feed_receipts)
                tree = monitors.pop().finish() if monitors else None
                if tree is not None: load.save(args.output / f"bots-{count}/process-tree.json", tree)
            scheduler_path = args.output / f"bots-{count}/scheduler.json"
            if not scheduler_path.is_file() or "clock" not in report.get("final", {}):
                report.update(failure="complete final scheduler/clock receipt unavailable", language_groups={}, qualified=False)
                load.save(args.output / f"bots-{count}/report.json", report)
                cases.append(report)
                print(json.dumps({"bots":count,"failure":report["failure"]}), flush=True)
                continue
            scheduler = json.loads(scheduler_path.read_text(encoding="utf-8"))
            spec = json.loads((args.output / f"bots-{count}/recipe.json").read_text(encoding="utf-8"))
            diagnostics = diagnostic_metrics(args.output / f"bots-{count}/measurement-end-metrics.txt")
            start_metrics = diagnostic_metrics(args.output / f"bots-{count}/measurement-start-metrics.txt")
            runtime_after = runtime_metrics(active_case_output / "measurement-end-metrics.txt")
            runtime_final = runtime_metrics(active_case_output / "metrics.txt")
            runtime_before = runtime_metrics(active_case_output / "measurement-start-metrics.txt")
            paths = runtime_delta(runtime_after, runtime_before)
            paths["requested_pine_route"] = "direct" if args.pine_in_process else "process"
            paths["requested_route_calls"] = sum(row["count"] for row in paths["routes"]
                if row["route"] == paths["requested_pine_route"])
            paths["pine_fallback_calls"] = sum(row["count"] for row in paths["routes"]
                if row["route"] == "blocked" or (row["route"] == "process" and row["reason"] != "none")) if args.pine_in_process else 0
            paths["requested_route_exercised"] = (paths["measurement_window_available"]
                and not paths["counter_reset"] and paths["requested_route_calls"] > 0
                and paths["pine_fallback_calls"] == 0)
            participant_sources = {spec["scenario"]["room_id"]:{agent["Plugin"]["participant"]["participant_id"]:
                agent["Plugin"]["config"]["script"] for agent in spec["agents"]
                if agent["Plugin"]["plugin_id"] == "pine.strategy"}}
            receipts = audit_receipts(active_case_output / "bot-receipts.jsonl",
                start_wall_us=runtime_before["wall_us"], end_wall_us=runtime_after["wall_us"],
                max_accepted_p95_ms=args.max_bar_close_to_admission_ms,
                participant_sources=participant_sources,
                require_sustained_coverage=sustained_coverage_required(args)) if (args.bot_trace or args.require_bot_receipts) else None
            if receipts is not None:
                receipts["trace_errors"] = runtime_final["trace_errors"]
                receipts["measurement_window_available"] = runtime_before["wall_us"] is not None and runtime_after["wall_us"] is not None
                receipts["qualified"] = (receipts["qualified"] and runtime_final["trace_enabled"]
                    and runtime_final["trace_errors"] == 0 and receipts["measurement_window_available"]
                    and (not args.trace_graceful_shutdown or report.get("shutdown", {}).get("qualified") is True))
                load.save(active_case_output / "bot-receipts-audit.json", receipts)
            diagnostics["measurement_deltas"] = {
                name: {"count":stage["count"]-start_metrics["stages"][name]["count"],
                       "total_wall_ms":stage["total_wall_ms"]-start_metrics["stages"][name]["total_wall_ms"]}
                for name, stage in diagnostics.get("stages", {}).items() if name in start_metrics.get("stages", {})}
            diagnostics["measurement_delta_scope"] = "scheduled warmup-end through scheduled measurement-end metrics; excludes drain; P95/max remain cumulative values"
            status = report["final"].get("status", {})
            duration = duration_summary(active_case_output / "samples.json", tree, args.warmup, args.seconds, 1000 / args.interval_ms)
            capture = {}
            for label, expected_time in (("start", args.warmup), ("end", args.warmup+args.seconds)):
                capture_path = active_case_output / f"measurement-{label}.json"
                if capture_path.is_file():
                    capture[label] = json.loads(capture_path.read_text(encoding="utf-8"))
                    capture[label]["expected_second"] = expected_time
                    capture[label]["within_half_second"] = abs(capture[label]["elapsed_after_start"]-expected_time) <= .5
            cutoff_room = spec["scenario"]["room_id"]
            cutoff_time = (receipts or {}).get("cutoff_market_by_room", {}).get(cutoff_room)
            progress = (receipts or {}).get("at_cutoff_progress", {}).get(cutoff_room, {})
            cutoff_lags, cutoff_missing = [], []
            if cutoff_time is not None:
                for agent in scheduler["agents"]:
                    config = agent["template"]["Plugin"]
                    if config["plugin_id"] not in ("pine.strategy", "python.sma"): continue
                    participant = config["participant"]
                    identifier = participant["participant_id"]
                    committed = progress.get(identifier)
                    if committed is None:
                        cutoff_missing.append(identifier)
                        continue
                    key = f'{participant["instrument_id"]}:{config["config"]["bar_interval_ms"]}'
                    expected = sum(close <= cutoff_time for close in scheduler["benchmark_required_inputs"][key]["closed_bar_close_times_ms"])
                    cutoff_lags.append(max(0, expected-committed["evaluated_bars"]))
            cutoff = {"market_time_ms":cutoff_time, "observed_instances":len(cutoff_lags),
                "missing_participants":cutoff_missing, "instances_behind":sum(lag > 0 for lag in cutoff_lags),
                "backlog_bars":{key.removesuffix("_ms"):value for key,value in load.percentiles(cutoff_lags).items()} if cutoff_lags else None,
                "scope":"required closed bars at scheduled cutoff versus per-decision durable publication at or before cutoff; post-cutoff drain cannot erase backlog"}
            groups = {}
            for agent in scheduler["agents"]:
                config = agent["template"]["Plugin"]
                language = "pine" if config["plugin_id"] == "pine.strategy" else "python" if config["plugin_id"] == "python.sma" else "native"
                group = groups.setdefault(language, {"instances":0,"initialized":0,"evaluated_bars":0,"skipped_bar_decisions":0,"lags_sim_ms":[],"bar_backlogs":[]})
                group["instances"] += 1
                data = agent["kind_state"].get("Plugin", {}).get("data")
                group["initialized"] += bool(data)
                if isinstance(data, dict):
                    group["evaluated_bars"] += data.get("evaluated_bars", 0)
                    group["skipped_bar_decisions"] += data.get("skipped_bar_decisions", 0)
                observed = data.get("_host_observation", data.get("last_observation")) if isinstance(data, dict) else None
                if observed is not None: group["lags_sim_ms"].append(report["final"]["clock"]["clock"]["market_time_ms"] - observed[1])
                if language != "native":
                    expected = scheduler["benchmark_required_inputs"][f'{config["participant"]["instrument_id"]}:{config["config"]["bar_interval_ms"]}']["closed_bars"]
                    group["bar_backlogs"].append(max(0, expected - (data.get("evaluated_bars", 0) if isinstance(data, dict) else 0)))
            for group in groups.values():
                lags = group.pop("lags_sim_ms")
                group["observed"] = len(lags)
                group["lag_sim_ms"] = load.percentiles(lags)
                backlogs = group.pop("bar_backlogs")
                group["closed_bar_backlog"] = {k.removesuffix("_ms"):v for k,v in load.percentiles(backlogs).items()} if backlogs else None
                group["instances_behind_closed_bars"] = sum(b > 0 for b in backlogs)
            report["qualified"].update(literal_step_rate=report["steps_per_wall_second"] is not None and report["steps_per_wall_second"] >= 1000 / args.interval_ms,
                all_languages_initialized=len(groups) == (2 if args.population == "pine" else 3) and all(g["initialized"] == g["instances"] for g in groups.values()),
                no_bot_errors="status" in report["final"] and not status.get("bot_errors"),
                recent_nonempty_input=all(v["latest_open_ms"] is not None and
                    v["market_time_ms"] - v["latest_open_ms"] <= 2 * int(k.rsplit(":", 1)[1])
                    for k,v in scheduler["benchmark_required_inputs"].items()),
                no_input_feed_errors=all(not f["errors"] for f in feed_receipts))
            report["qualified"].update(
                no_closed_bar_backlog_after_pause=all(g["instances_behind_closed_bars"] == 0 for name, g in groups.items() if name != "native"),
                no_closed_bar_backlog_at_cutoff=(bool(cutoff_lags) and not cutoff_missing and cutoff["instances_behind"] == 0)
                    if args.require_bot_receipts else None,
                no_skipped_bar_decisions=all(g["skipped_bar_decisions"] == 0 for name, g in groups.items() if name != "native"),
                all_pine_lookbacks_exercised=all(
                    (agent["kind_state"].get("Plugin", {}).get("data") or {}).get("evaluated_bars", 0) >=
                    max((agent["template"]["Plugin"]["config"].get("inputs", {}).get(key, default)
                         for key, default in (("Fast", 3), ("Slow", 8), ("Length", 7))), default=8) + 1
                    for agent in scheduler["agents"] if agent["template"]["Plugin"]["plugin_id"] == "pine.strategy"),
                all_horizon_groups_exercised=all(v["closed_bars"] > 0 for v in scheduler["benchmark_required_inputs"].values()),
                compiler_quiet=tree.get("compiler_nonempty_samples") == 0 if tree else False,
                no_parallel_benchmark_server=tree.get("other_server_nonempty_samples") == 0 if tree else False,
                diagnostic_stages_present=bool(diagnostics.get("stages", {}).get(
                    "bot_inprocess_call" if args.pine_in_process else "bot_worker_roundtrip"))
                    if args.require_diagnostics else None,
                requested_runtime_route=paths["requested_route_exercised"]
                    if (args.require_diagnostics or args.require_bot_receipts) else None,
                bot_durable_receipts=receipts["qualified"] if args.require_bot_receipts else None)
            report["qualified"].update(full_requested_duration_observed=duration["full_requested_duration_observed"],
                sustained_target=duration["sustained_target"] if args.long_run else None,
                no_harness_errors=not report["errors"],
                process_sampling_complete=duration["process_sampling_complete"],
                no_multi_second_clock_stall=duration["longest_observed_no_progress_seconds"] < 2 if args.long_run else None,
                metric_window_captured=len(capture) == 2 and all(row["within_half_second"] for row in capture.values()),
                long_run_cutoff_receipts=bool(receipts and receipts["measurement_window_available"])
                    if args.long_run else None,
                long_run_cycle_input=args.input_profile == "cycle" if args.long_run else None,
                sustained_accepted_coverage=receipts["accepted_window_coverage"]["qualified"]
                    if sustained_coverage_required(args) else None,
                pine_checkpoint_schema=all((a["kind_state"].get("Plugin", {}).get("data") or {}).get("broker_schema") == 3
                    for a in scheduler["agents"] if a["template"]["Plugin"]["plugin_id"] == "pine.strategy") if args.pine_vm_checkpoint else None)
            report.update(language_groups=groups, process_tree={k:v for k,v in tree.items() if k != "samples"} if tree else None,
                workload=workload_summary(spec), pine_profile=args.pine_profile, start_step=args.start_step,
                prewarm_bars=args.prewarm_bars, warmup_seconds=args.warmup,
                event_prefilter_enabled=args.event_prefilter,
                pine_vm_checkpoint_enabled=args.pine_vm_checkpoint,
                long_run=args.long_run, max_memory_mib=args.max_memory_mib, duration=duration,
                metric_window=capture, cutoff_progress=cutoff,
                diagnostics=diagnostics,
                runtime_paths=paths,
                bot_durable_receipts=receipts,
                input_profile=args.input_profile,
                feed_execution=feed_execution_summary(feed_receipts),
                pine_ledger_fills=pine_ledger_fill_summary(scheduler),
                input_activity=scheduler["benchmark_required_inputs"],
                sustained_input=args.sustained_input,
                probe="account 900000 sell at 101, quantity 1, then cancel on separate empty LOAD-PROBE-SPOT book",
                population=args.population,
                scope="actual Pine count plus 38 fixed native liquidity bots; three qualified sources" if args.population == "pine" else "80% native, 10% first-party Python SMA, 10% qualified Pine; not community-library capacity")
            load.save(args.output / f"bots-{count}/report.json", report)
            cases.append(report)
            print(json.dumps({"bots":count,"steps_per_second":report["steps_per_wall_second"],"groups":groups,
                "errors":report["errors"],"bot_errors":status.get("bot_errors")}, default=str), flush=True)
        load.save(args.output / "report.json", {"cases":cases,"qualification":f"{'sustained' if args.long_run else 'short bounded'} {args.population} load; in-memory durable journal; PostgreSQL capacity and crash recovery require separate evidence"})
    finally:
        load.Client = original_client
        load.subprocess.Popen = popen
        load.load_recipe = ORIGINAL
        if previous is None: os.environ.pop("MARKETFORGE_BOT_PLUGIN_DIR", None)
        else: os.environ["MARKETFORGE_BOT_PLUGIN_DIR"] = previous
        if previous_prefilter is None: os.environ.pop("MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER", None)
        else: os.environ["MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER"] = previous_prefilter


if __name__ == "__main__": main()
