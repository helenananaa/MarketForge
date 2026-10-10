"""Measure isolated realtime service load with finite accounts and bounded probes."""
import argparse
import copy
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import threading
import time
import urllib.request

from microstructure_market import microstructure_recipe
from background_market import child_seed, Client
from validate_behavior_market_live import port, peak_memory, save

ROOT=Path(__file__).resolve().parents[1]


def load_recipe(count,room="load-market",seed=7,staggered=False,template=None):
    if not 38<=count<=1000:raise ValueError("population must be within 38..1000")
    spec=microstructure_recipe(room,seed) if template is None else copy.deepcopy(template)
    spec["scenario"]["room_id"]=room
    for agent in spec["agents"]:
        bot=agent["Plugin"]
        bot["participant"]["room_id"]=room
        bot["seed"]=max(1,child_seed(seed,bot["participant"]["participant_id"])&(2**53-1))
    noise={leg:next(a for a in spec["agents"] if a["Plugin"]["plugin_id"]=="AdaptiveNoiseTrader"
                   and a["Plugin"]["participant"]["instrument_id"].endswith(leg)) for leg in ("SPOT","PERP")}
    accounts={next(iter(a.values()))["account_id"]:a for a in spec["scenario"]["accounts"]}
    for i in range(count-38):
        leg="SPOT" if i%2==0 else "PERP"
        agent=copy.deepcopy(noise[leg]);bot=agent["Plugin"]
        old=bot["participant"]["account_id"];account=10000+i;name=f"load-{leg.lower()}-{i}"
        allocation=copy.deepcopy(accounts[old]);next(iter(allocation.values()))["account_id"]=account
        bot["participant"].update(account_id=account,participant_id=name)
        bot["seed"]=max(1,child_seed(seed,name)&(2**53-1))
        spec["scenario"]["accounts"].append(allocation);spec["agents"].append(agent)
    # Keep available maker depth per participant comparable across population
    # tiers. Allocations grow explicitly at initialization, never during trading.
    scale=count/38
    for agent in spec["agents"]:
        bot=agent["Plugin"]
        if bot["plugin_id"]=="DynamicMarketMaker":
            config=bot["config"]
            for name in ("max_qty","inventory_cap","inventory_target","toxic_flow_min_qty"):
                if name in config:config[name]=math.ceil(config[name]*scale)
            allocation=accounts[bot["participant"]["account_id"]]
            for name in ("cash_balance","position_qty"):
                data=next(iter(allocation.values()))
                if name in data:data[name]=math.ceil(data[name]*scale)
    if staggered:
        for agent in spec["agents"]:
            bot=agent["Plugin"]
            if bot["plugin_id"]=="AdaptiveNoiseTrader":
                bot["config"].update(decision_interval_ms=750+child_seed(seed,bot["participant"]["participant_id"])%500,jitter_ms=500)
    spec["scenario"]["accounts"].append({"Spot":{"account_id":900000,"cash_balance":100000,"position_qty":100}})
    spec["agent_interval_ms"]=25
    return spec


def percentiles(values):
    values=sorted(values)
    def q(p):
        if not values:return None
        return values[min(len(values)-1,math.ceil(len(values)*p)-1)]
    return {"count":len(values),"p50_ms":q(.5),"p95_ms":q(.95),"max_ms":q(1)}


def cpu_seconds(process):
    """Kernel + user CPU time for this child only; unavailable is not zero."""
    if os.name != "nt":return None
    import ctypes
    from ctypes import wintypes
    times=[wintypes.FILETIME() for _ in range(4)]
    call=ctypes.windll.kernel32.GetProcessTimes
    call.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
    call.restype=wintypes.BOOL
    if not call(wintypes.HANDLE(int(process._handle)),*[ctypes.byref(t) for t in times]):return None
    return sum((t.dwHighDateTime<<32)+t.dwLowDateTime for t in times[2:])/10_000_000


def observation_freshness(scheduler, market_time_ms):
    """Final durable observations, not decision throughput or a steady-state SLO."""
    lags=[]
    for agent in scheduler["agents"]:
        data=agent.get("kind_state",{}).get("Plugin",{}).get("data")
        observation=data.get("last_observation") if isinstance(data,dict) else None
        if observation is not None:
            lags.append(market_time_ms-observation[1])
    return {"definition":"paused final durable snapshot; simulation time lag, not wall-clock latency",
            "bots_with_observation":len(lags),"bots_without_observation":len(scheduler["agents"])-len(lags),
            "lag_sim_ms":percentiles(lags),"future_observations":sum(lag<0 for lag in lags)}


def run_case(args,count):
    output=args.output/f"bots-{count}"
    output.mkdir(parents=True,exist_ok=True)
    room=f"load-{count}"
    template=json.loads(args.recipe_template.read_text(encoding="utf-8")) if args.recipe_template else None
    spec=load_recipe(count,room,args.seed,args.staggered,template)
    interval=getattr(args,"interval_ms",25)
    spec["agent_interval_ms"]=interval
    save(output/"recipe.json",spec)
    url=f"http://127.0.0.1:{port()}"
    env={k:v for k,v in os.environ.items() if not k.startswith(("MARKETFORGE_","PG"))}
    env.update(MARKETFORGE_BIND_ADDR=url.removeprefix("http://"),MARKETFORGE_CORS_ORIGINS="http://127.0.0.1:15281",
               PATH=str(Path(sys.executable).parent)+os.pathsep+env["PATH"])
    # No auth is configured in the isolated process, matching the web's local
    # development flow. It binds loopback and uses a fresh working directory.
    stop=threading.Event();samples=[];latencies={"view":[],"place":[],"cancel":[]};errors=[];resources=[]
    with (output/"server.log").open("w",encoding="utf-8") as log:
        process=subprocess.Popen([str(args.server.resolve())],cwd=output.resolve(),env=env,stdout=log,stderr=subprocess.STDOUT,
                                 creationflags=subprocess.CREATE_NO_WINDOW if os.name=="nt" else 0)
        client=Client(url,timeout=5)
        try:
            deadline=time.perf_counter()+25
            while True:
                try:client.health_ready();break
                except (OSError,RuntimeError):
                    if process.poll() is not None or time.perf_counter()>deadline:raise RuntimeError("server readiness failed")
                    time.sleep(.05)
            if args.browser_wait:
                save(output/"connection.json",{"url":url,"room":room,"pid":process.pid})
            client._request("POST","/rooms",spec)
            begun=time.perf_counter();measured_after=begun+args.warmup
            def guard():
                while not stop.wait(.2):
                    if process.poll() is not None:return
                    memory=peak_memory(process)
                    resources.append({"elapsed":time.perf_counter()-begun,"peak_bytes":memory,"cpu_seconds":cpu_seconds(process)})
                    if memory and memory>args.max_memory_mib*1024**2:
                        errors.append({"kind":"memory_guard","peak_bytes":memory});stop.set();return
            def observe():
                probe=Client(url,timeout=3)
                while not stop.is_set():
                    start=time.perf_counter()
                    try:
                        clock=probe.clock(room)["clock"]
                        samples.append({"elapsed":time.perf_counter()-begun,"step":clock["step"],"clock_latency_ms":(time.perf_counter()-start)*1000})
                    except Exception as e:errors.append({"kind":"clock","error":str(e)})
                    stop.wait(.25)
            def orders():
                probe=Client(url,timeout=3)
                while not stop.is_set():
                    try:
                        start=time.perf_counter();result=probe.place(room,900000,"Buy",1,1)
                        if measured_after<=start<measured_after+args.seconds:latencies["place"].append((time.perf_counter()-start)*1000)
                        if not result.get("accepted"):raise RuntimeError(f"probe rejected: {result}")
                        order=next(e["order_id"] for e in result["events"] if e["type"]=="OrderRested")
                        start=time.perf_counter();result=probe.cancel(room,900000,order)
                        if measured_after<=start<measured_after+args.seconds:latencies["cancel"].append((time.perf_counter()-start)*1000)
                        if not result.get("accepted"):raise RuntimeError(f"cancel rejected: {result}")
                    except Exception as e:errors.append({"kind":"order","error":str(e)})
                    stop.wait(.5)
            def views():
                probe=Client(url,timeout=3)
                while not stop.is_set():
                    start=time.perf_counter()
                    try:
                        probe._request("GET",f"/rooms/{room}/view")
                        if measured_after<=start<measured_after+args.seconds:latencies["view"].append((time.perf_counter()-start)*1000)
                    except Exception as e:errors.append({"kind":"view","error":str(e)})
                    stop.wait(.5)
            workers=[threading.Thread(target=f,daemon=True) for f in (guard,observe,orders,views)]
            for worker in workers:worker.start()
            # Browser attachment receives a concrete local URL and can run its
            # own visible-page frame/long-task measurements during this window.
            duration=args.warmup+args.seconds+args.browser_wait
            while not stop.wait(.2) and time.perf_counter()-begun<duration:
                if process.poll() is not None:errors.append({"kind":"server_exit","code":process.returncode});break
            stop.set()
            for worker in workers:worker.join(timeout=4)
            final={}
            if process.poll() is None:
                try:
                    client.pause_room(room)
                    final["clock"]=client.clock(room)
                    final["status"]=client._request("GET",f"/rooms/{room}/agents")
                    req=urllib.request.Request(url+"/metrics")
                    with urllib.request.urlopen(req,timeout=5) as response:
                        (output/"metrics.txt").write_bytes(response.read())
                    saved=client.room_bots(room)
                    save(output/"scheduler.json",saved)
                    final["active_bots"]=sum(a["kind_state"]["Plugin"]["data"].get("last_observation") is not None for a in saved["agents"])
                    final["observation_freshness"]=observation_freshness(saved,final["clock"]["clock"]["market_time_ms"])
                except Exception as e:errors.append({"kind":"final_receipt","error":str(e)})
            save(output/"samples.json",{"clock":samples,"latencies":latencies,"memory":resources,"errors":errors})
            measured=[s for s in samples if args.warmup<=s["elapsed"]<=args.warmup+args.seconds]
            rate=(measured[-1]["step"]-measured[0]["step"])/(measured[-1]["elapsed"]-measured[0]["elapsed"]) if len(measured)>1 else None
            cpu=[s for s in resources if args.warmup<=s["elapsed"]<=args.warmup+args.seconds and s["cpu_seconds"] is not None]
            cores=(cpu[-1]["cpu_seconds"]-cpu[0]["cpu_seconds"])/(cpu[-1]["elapsed"]-cpu[0]["elapsed"]) if len(cpu)>1 else None
            result={"bots":count,"build":"release","server_sha256":hashlib.sha256(args.server.read_bytes()).hexdigest(),
                    "recipe_sha256":hashlib.sha256((output/"recipe.json").read_bytes()).hexdigest(),
                    "observed_bots_definition":"final active_bots counts participants with an observation, not simultaneous orders",
                    "interval_ms":interval,"target_steps_per_second":1000/interval,"warmup_seconds":args.warmup,"measurement_seconds":args.seconds,"steps_per_wall_second":rate,
                    "average_logical_cores":cores,"logical_processors":os.cpu_count(),
                    "latency":{k:percentiles(v) for k,v in latencies.items()},"peak_memory_mib":max((r["peak_bytes"] or 0 for r in resources),default=0)/1024**2,
                    "clock_latency":percentiles([s["clock_latency_ms"] for s in measured]),"errors":errors,"final":final,
                    "staggered":args.staggered,"storage":"in-memory durable journal; not PostgreSQL",
                    "qualified":{"order_p95_under_250ms":all(latencies[k] and percentiles(latencies[k])["p95_ms"]<=250 for k in ("place","cancel")) and not any(e["kind"]=="order" for e in errors),
                                 "configured_step_rate":rate is not None and rate>=.95*1000/interval,
                                 "configured_40_steps_per_second":(rate is not None and rate>=38) if interval==25 else None}}
            save(output/"report.json",result)
            return result
        finally:
            stop.set()
            if process.poll() is None:
                process.terminate()
                try:process.wait(timeout=10)
                except subprocess.TimeoutExpired:process.kill();process.wait(timeout=5)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--counts",type=int,nargs="+",default=[38,100,200,500])
    parser.add_argument("--seconds",type=float,default=20)
    parser.add_argument("--warmup",type=float,default=3)
    parser.add_argument("--interval-ms",type=int,default=25,help="wall-clock interval per 1000ms simulation step; 1000 is realtime")
    parser.add_argument("--seed",type=int,default=7)
    parser.add_argument("--max-memory-mib",type=float,default=1024)
    parser.add_argument("--staggered",action="store_true")
    parser.add_argument("--recipe-template",type=Path,help="freeze a baseline 38-bot recipe for comparable engine measurements")
    parser.add_argument("--browser-wait",type=float,default=0)
    args=parser.parse_args()
    if not 25<=args.interval_ms<=1000:parser.error("interval must be 25..1000ms")
    if not 5<=args.seconds<=120 or not 0<=args.warmup<=30:parser.error("bounded 5..120s measurement and 0..30s warmup required")
    if not 0<=args.browser_wait<=120 or not 0<args.max_memory_mib<=4096:parser.error("bounded 0..120s browser window and finite memory limit up to 4096MiB required")
    args.output.mkdir(parents=True,exist_ok=True)
    reports=[]
    for count in args.counts:
        result=run_case(args,count);reports.append(result)
        print(json.dumps({"bots":count,"steps_per_second":result["steps_per_wall_second"],"peak_mib":result["peak_memory_mib"],"latency":result["latency"],"errors":result["errors"]}),flush=True)
    save(args.output/"report.json",{"cases":reports,"thresholds":{"order_p95_ms":250,"memory_guard_mib":args.max_memory_mib},"qualification":"bounded local synthetic load measurement; no long-duration capacity guarantee"})


if __name__=="__main__":main()
