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
sys.path.insert(0, str(ROOT / "python"))
import benchmark_market_load as load
from background_market import child_seed
from process_tree_metrics import ProcessTreeMetrics


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


class SustainedInput:
    """Finite funded traders create real fills through the exchange gateway."""
    def __init__(self, client, room):
        self.client, self.room = client, room
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
                    if number >= 2000: raise RuntimeError("finite input feed exhausted")
                    responses = []
                    for side, account in (("Sell", 900001), ("Buy", 900002)):
                        result = self.client._request("POST", f"/rooms/{self.room}/instruments/V-BTC-SPOT/orders",
                            {"participant_id":f"load-feed-{side}", "account_id":account,
                             "action":{"PlaceLimit":{"side":side,"price_tick":100,"qty":1}}},
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
                    self.receipts.append({"clock":clock, "orders":responses})
                self.stop.wait(.05)
        except Exception as error:
            self.errors.append(repr(error))

    def finish(self):
        self.stop.set()
        self.thread.join(timeout=10)
        if self.thread.is_alive(): raise RuntimeError("input feed did not stop")
        return {"scope":"real gateway limit orders at tick 100; two finite funded accounts; no injected candles",
                "iterations":len(self.receipts), "errors":self.errors, "receipts":self.receipts}


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
    parser.add_argument("--pine-in-process", action="store_true", help="enable the optional qualified Rust Pine backend")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--counts", type=int, nargs="+", default=[100,500,1000,3000])
    parser.add_argument("--seconds", type=float, default=30)
    parser.add_argument("--interval-ms", type=int, default=25)
    parser.add_argument("--plugin-dir", type=Path, default=ROOT / "bot-plugins")
    parser.add_argument("--population", choices=("mixed", "pine"), default="mixed",
                        help="pine counts actual Pine bots plus 38 fixed native liquidity providers")
    parser.add_argument("--sustained-input", action="store_true",
                        help="pure Pine only: funded gateway traders keep nonempty candles arriving")
    parser.add_argument("--pine-profile", choices=("short", "diverse-short", "mixed-horizon", "slow-horizon"), default="short",
                        help="mixed-horizon: 60%% 1m, 30%% 5m, 10%% 1h; slow-horizon: 50%% 5m, 50%% 1h; assumed workloads, not captured production traffic")
    parser.add_argument("--start-step", type=int, default=0,
                        help="advance empty simulation before starting bots; 3590 exercises common hour close shortly after startup, not long-history acceptance")
    parser.add_argument("--require-diagnostics", action="store_true",
                        help="fail qualification unless server was built with bench-execution-timing")
    parser.add_argument("--event-prefilter", action="store_true",
                        help="opt in to the experimental host event prefilter; default disabled")
    parser.add_argument("--prewarm-bars", type=int, default=0,
                        help="create real gateway history before starting bots: this many bars at the longest configured period; no injected candles")
    parser.add_argument("--warmup", type=float, default=5,
                        help="exclude this many wall seconds after bot startup from clock/probe measurements (0..30)")
    parser.add_argument("--export-only", action="store_true", help="freeze recipes and workload summaries without launching a server")
    args = parser.parse_args()
    if not 5 <= args.seconds <= 120 or not 25 <= args.interval_ms <= 1000: parser.error("bounded duration/clock required")
    if args.sustained_input and args.population != "pine": parser.error("sustained input requires pure Pine")
    if not 0 <= args.start_step <= 10000: parser.error("start-step must be within 0..10000")
    if not 0 <= args.prewarm_bars <= 128 or not 0 <= args.warmup <= 30: parser.error("bounded prewarm-bars/warmup required")
    if args.prewarm_bars and (args.population != "pine" or not args.sustained_input or args.start_step):
        parser.error("history prewarming requires pure Pine sustained input and start-step 0")
    if args.population != "pine" and (args.pine_profile != "short" or args.start_step):
        parser.error("Pine workload and start-step options require pure Pine")
    args.output.mkdir(parents=True, exist_ok=True)
    # A scoped child environment installs only these trusted local packages.
    previous = os.environ.get("MARKETFORGE_BOT_PLUGIN_DIR")
    os.environ["MARKETFORGE_BOT_PLUGIN_DIR"] = str(args.plugin_dir.resolve())
    previous_prefilter = os.environ.get("MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER")
    os.environ["MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER"] = "1" if args.event_prefilter else "0"
    def recipe(*values, **kwargs):
        spec = pine_recipe(*values, **kwargs, profile=args.pine_profile) if args.population == "pine" else mixed_recipe(*values, **kwargs)
        if args.sustained_input:
            for account, position in ((900001, 10000), (900002, 0)):
                spec["scenario"]["accounts"].append({"Spot":{"account_id":account,
                    "cash_balance":10000000,"position_qty":position}})
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
    def installed_popen(*values, **kwargs):
        if kwargs.get("env", {}).get("MARKETFORGE_BIND_ADDR"):
            kwargs["env"]["MARKETFORGE_PINE_IN_PROCESS"] = "1" if args.pine_in_process else "0"
            kwargs["env"]["MARKETFORGE_BOT_PLUGIN_DIR"] = str(args.plugin_dir.resolve())
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
                        feeds.append(SustainedInput(original_client(self.base_url, timeout=3), room))
                    origin = time.perf_counter()
                    if monitors:
                        start = origin - monitors[-1].begun + args.warmup
                        monitors[-1].measurement_window = (start, start + args.seconds)
                    def capture_start_metrics():
                        try:
                            body = load.urllib.request.urlopen(self.base_url + "/metrics", timeout=3).read().decode("utf-8")
                            (case_output / "measurement-start-metrics.txt").write_text(body, encoding="utf-8")
                            load.save(case_output / "measurement-start.json", {"elapsed_after_start":time.perf_counter()-origin})
                        except Exception as error:
                            load.save(case_output / "measurement-start-error.json", {"error":repr(error)})
                    timer = threading.Timer(args.warmup, capture_start_metrics)
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
                    closed = [bar for bar in page["candles"] if bar["is_final"]]
                    if len(page["candles"]) == 2000: raise RuntimeError("required-bar audit may be truncated")
                    required[key] = {"closed_bars":len(closed), "latest_open_ms":closed[-1]["open_time_ms"] if closed else None,
                                     "market_time_ms":page["market_time_ms"]}
            saved["benchmark_required_inputs"] = required
            return saved
    load.Client = RequiredInputClient
    try:
        cases = []
        for count in args.counts:
            run_args = SimpleNamespace(**vars(args), recipe_template=None, seed=7, staggered=False,
                browser_wait=0, max_memory_mib=4096)
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
            diagnostics = diagnostic_metrics(args.output / f"bots-{count}/metrics.txt")
            start_metrics = diagnostic_metrics(args.output / f"bots-{count}/measurement-start-metrics.txt")
            diagnostics["measurement_deltas"] = {
                name: {"count":stage["count"]-start_metrics["stages"][name]["count"],
                       "total_wall_ms":stage["total_wall_ms"]-start_metrics["stages"][name]["total_wall_ms"]}
                for name, stage in diagnostics.get("stages", {}).items() if name in start_metrics.get("stages", {})}
            diagnostics["measurement_delta_scope"] = "counter snapshot after warmup through final paused metrics; includes final quiescence; P95/max remain whole-run values"
            status = report["final"].get("status", {})
            groups = {}
            for agent in scheduler["agents"]:
                config = agent["template"]["Plugin"]
                language = "pine" if config["plugin_id"] == "pine.strategy" else "python" if config["plugin_id"] == "python.sma" else "native"
                group = groups.setdefault(language, {"instances":0,"initialized":0,"evaluated_bars":0,"lags_sim_ms":[],"bar_backlogs":[]})
                group["instances"] += 1
                data = agent["kind_state"].get("Plugin", {}).get("data")
                group["initialized"] += bool(data)
                if isinstance(data, dict): group["evaluated_bars"] += data.get("evaluated_bars", 0)
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
                no_closed_bar_backlog=all(g["instances_behind_closed_bars"] == 0 for name, g in groups.items() if name != "native"),
                all_pine_lookbacks_exercised=all(
                    (agent["kind_state"].get("Plugin", {}).get("data") or {}).get("evaluated_bars", 0) >=
                    max((agent["template"]["Plugin"]["config"].get("inputs", {}).get(key, default)
                         for key, default in (("Fast", 3), ("Slow", 8), ("Length", 7))), default=8) + 1
                    for agent in scheduler["agents"] if agent["template"]["Plugin"]["plugin_id"] == "pine.strategy"),
                all_horizon_groups_exercised=all(v["closed_bars"] > 0 for v in scheduler["benchmark_required_inputs"].values()),
                compiler_quiet=tree.get("compiler_nonempty_samples") == 0 if tree else False,
                no_parallel_benchmark_server=tree.get("other_server_nonempty_samples") == 0 if tree else False,
                diagnostic_stages_present=bool(diagnostics.get("stages", {}).get("bot_worker_roundtrip"))
                    if args.require_diagnostics else None)
            report.update(language_groups=groups, process_tree={k:v for k,v in tree.items() if k != "samples"} if tree else None,
                workload=workload_summary(spec), pine_profile=args.pine_profile, start_step=args.start_step,
                prewarm_bars=args.prewarm_bars, warmup_seconds=args.warmup,
                event_prefilter_enabled=args.event_prefilter,
                diagnostics=diagnostics,
                input_activity=scheduler["benchmark_required_inputs"],
                sustained_input=args.sustained_input,
                probe="account 900000 sell at 101, quantity 1, then cancel on separate empty LOAD-PROBE-SPOT book",
                population=args.population,
                scope="actual Pine count plus 38 fixed native liquidity bots; three qualified sources" if args.population == "pine" else "80% native, 10% first-party Python SMA, 10% qualified Pine; not community-library capacity")
            load.save(args.output / f"bots-{count}/report.json", report)
            cases.append(report)
            print(json.dumps({"bots":count,"steps_per_second":report["steps_per_wall_second"],"groups":groups,
                "errors":report["errors"],"bot_errors":status.get("bot_errors")}, default=str), flush=True)
        load.save(args.output / "report.json", {"cases":cases,"qualification":f"short bounded {args.population} load; long-run memory and PostgreSQL capacity unverified"})
    finally:
        load.Client = original_client
        load.subprocess.Popen = popen
        load.load_recipe = ORIGINAL
        if previous is None: os.environ.pop("MARKETFORGE_BOT_PLUGIN_DIR", None)
        else: os.environ["MARKETFORGE_BOT_PLUGIN_DIR"] = previous
        if previous_prefilter is None: os.environ.pop("MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER", None)
        else: os.environ["MARKETFORGE_EXPERIMENTAL_EVENT_PREFILTER"] = previous_prefilter


if __name__ == "__main__": main()
