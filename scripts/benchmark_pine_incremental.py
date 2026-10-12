"""ABBA fixed-input Pine evaluation comparison; no live exchange fills or async throughput."""
import argparse
import copy
import hashlib
import json
from pathlib import Path
import random
import sys
import time
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path[:0] = [str(ROOT / "bot-plugins/pine-strategy/.runtime"), str(ROOT / "python/tests"), str(ROOT / "python")]
from test_pine_bot import bot
from test_pine_broker import broker_request, perp


def run(mode, count):
    incremental = bot.incremental_module()
    incremental.SESSIONS.clear()
    counters = dict(incremental.COUNTERS)
    digest = hashlib.sha256()
    started = time.perf_counter()
    rng = random.Random(7)
    prices = [100 + rng.randrange(-8, 9) for _ in range(count)]
    sources = json.loads((ROOT / "bot-plugins/pine-strategy/incremental-qualified.json").read_text())["source_sha256"]
    for script in sources:
        state = None
        for end in range(1, count + 1):
            req = broker_request(prices[:end]);req["config"]["script"] = script
            if script == "perp.pine": req = perp(req)
            if script == "basis.pine":
                req["config"]["related_instruments"] = ["V-BTC-PERP"]
                req["observation"]["related_markets"] = [copy.deepcopy(perp(broker_request([p+3 for p in prices[:end]]))["observation"])]
            req["state"] = state
            result = bot.decide(req)
            digest.update(json.dumps(result, sort_keys=True, separators=(",", ":")).encode())
            state = json.loads(json.dumps(result["state"]))
    return {"mode":mode, "seconds":time.perf_counter()-started, "response_sha256":digest.hexdigest(),
            "decisions":count*len(sources), "counter_delta":{k:incremental.COUNTERS[k]-v for k,v in counters.items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bars", type=int, default=128)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 32 <= args.bars <= 256: parser.error("use 32..256 bars")
    import pine_compat
    if not hasattr(pine_compat.Program, "external_session"): raise RuntimeError("private incremental runtime required")
    incremental = bot.incremental_module()
    evaluate = incremental.evaluate
    results = []
    # Compile before either measurement; both paths use the same program cache.
    for script in json.loads((ROOT / "bot-plugins/pine-strategy/incremental-qualified.json").read_text())["source_sha256"]:
        bot.compile_program((ROOT / "bot-plugins/pine-strategy/scripts" / script).read_text(), {})
    for mode in ("replay", "incremental", "incremental", "replay"):
        selected = evaluate if mode == "incremental" else incremental.replay
        with patch.object(incremental, "evaluate", selected): results.append(run(mode, args.bars))
        print(json.dumps(results[-1]), flush=True)
    if len({r["response_sha256"] for r in results}) != 1: raise AssertionError("fixed decision/state equivalence failed")
    report = {"scope":"six qualified sources, synthetic frozen account feedback, complete response/state equality; no exchange fills or end-to-end throughput claim",
              "runtime":pine_compat.__version__, "bars_per_source":args.bars, "runs":results}
    replay = sum(r["seconds"] for r in results if r["mode"] == "replay")
    fast = sum(r["seconds"] for r in results if r["mode"] == "incremental")
    report["elapsed_reduction_percent"] = (1-fast/replay)*100
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2), encoding="utf-8")


if __name__ == "__main__": main()
