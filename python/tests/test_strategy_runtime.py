"""Worker recovery, frozen-prefix equivalence and bounded public feature reuse."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import random
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
from test_pine_bot import bot, request
from test_pine_broker import broker_request, perp


class IncrementalPineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import pine_compat
        if not hasattr(pine_compat.Program, "external_session"):
            if os.environ.get("MARKETFORGE_REQUIRE_INCREMENTAL_TESTS") == "1":
                raise AssertionError("private external-session runtime required")
            raise unittest.SkipTest("private external-session adapter not selected")

    def test_all_qualified_sources_match_frozen_replay_across_prefixes_and_recovery(self):
        incremental = bot.incremental_module()
        cases = json.loads((ROOT / "bot-plugins/pine-strategy/incremental-qualified.json").read_text())["source_sha256"]
        total = 0
        for script in cases:
            for seed in range(3):
                rng = random.Random(seed)
                prices = [100 + rng.randrange(-8, 9) for _ in range(32)]
                fast_state = slow_state = None
                incremental.SESSIONS.clear()
                for end in range(1, len(prices) + 1):
                    req = broker_request(prices[:end]);req["config"]["script"] = script
                    if script == "perp.pine": req = perp(req)
                    if script == "basis.pine":
                        req["config"]["related_instruments"] = ["V-BTC-PERP"]
                        req["observation"]["related_markets"] = [copy.deepcopy(perp(broker_request([p+3 for p in prices[:end]]))["observation"])]
                    req["state"] = copy.deepcopy(fast_state)
                    # Missed appends/restarts rebuild from the exact durable transcript.
                    if end in (13, 24): incremental.SESSIONS.clear()
                    fast = bot.decide(req)
                    req["state"] = copy.deepcopy(slow_state)
                    with patch.object(incremental, "evaluate", incremental.replay):
                        slow = bot.decide(req)
                    self.assertEqual(fast, slow, (script, seed, end))
                    fast_state = json.loads(json.dumps(fast["state"]))
                    slow_state = json.loads(json.dumps(slow["state"]))
                    total += 1
        self.assertEqual(total, 576)
        self.assertGreater(incremental.COUNTERS["appends"], 0)

    def test_uncommitted_session_is_rebuilt_and_changed_history_is_rejected(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        initial = bot.decide(broker_request([100, 101]))
        speculative = broker_request([100, 101, 102], state=initial["state"])
        expected = bot.decide(speculative)
        self.assertEqual(bot.decide(copy.deepcopy(speculative)), expected)
        changed = broker_request([90, 101, 102], state=initial["state"])
        with self.assertRaisesRegex(ValueError, "history changed"):
            bot.decide(changed)

    def test_session_cache_is_bounded_and_eviction_reconstructs_durable_transcript(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        first_req = broker_request([100, 101])
        first = bot.decide(first_req)
        for index in range(incremental.MAX_SESSIONS):
            req = copy.deepcopy(first_req)
            req["participant"]["participant_id"] = f"separate-{index}"
            bot.decide(req)
        self.assertEqual(len(incremental.SESSIONS), incremental.MAX_SESSIONS)
        req = broker_request([100, 101, 102], state=first["state"])
        actual = bot.decide(copy.deepcopy(req))
        with patch.object(incremental, "evaluate", incremental.replay):
            self.assertEqual(actual, bot.decide(req))

    def test_external_session_rejects_changed_boundary_and_unconfirmed_pass(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        first = bot.decide(broker_request([100, 101]))
        entry = next(iter(incremental.SESSIONS.values()))
        session = entry["session"]
        before = session.cursor
        evaluation = entry["evaluations"][-1]
        last_pass = [[{"account":entry["accounts"][-1], "bar":entry["bars"][-1],
            "event_time_ms":evaluation["time"], "confirmed":True,
            "request_data":[{"symbol":key.rsplit(":",1)[0], "timeframe":key.rsplit(":",1)[1],
                "bars":entry["providers"][key][:count]} for key,count in evaluation["providers"].items()]}]]
        with self.assertRaisesRegex(ValueError, "boundary"):
            session.append(entry["bars"][-1:], entry["accounts"][-1:], execution_passes=last_pass)
        passes = copy.deepcopy(last_pass);passes[-1][-1]["confirmed"] = False
        with self.assertRaisesRegex(ValueError, "confirmed external pass"):
            session.append(entry["bars"][-1:], entry["accounts"][-1:], execution_passes=passes)
        self.assertEqual(session.cursor, before)
        req = broker_request([100, 101, 102], state=first["state"])
        actual = bot.decide(copy.deepcopy(req))
        with patch.object(incremental, "evaluate", incremental.replay):
            self.assertEqual(actual, bot.decide(req))

    def test_warm_session_materializes_only_new_passes_and_checks_frozen_prefix(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        state = bot.decide(broker_request(list(range(100, 132))))["state"]
        starts = []
        original = incremental.evaluate
        def observed(program, source, request, *args, pass_factory, **kwargs):
            def measured(start):
                starts.append(start)
                return pass_factory(start)
            return original(program, source, request, *args, pass_factory=measured, **kwargs)
        req = broker_request(list(range(100, 133)), state=state)
        with patch.object(incremental, "evaluate", observed):
            actual = bot.decide(copy.deepcopy(req))
        self.assertEqual(starts, [32])
        entry = next(iter(incremental.SESSIONS.values()))
        self.assertNotIn("passes", entry)
        with patch.object(incremental, "evaluate", incremental.replay):
            self.assertEqual(actual, bot.decide(copy.deepcopy(req)))
        # A corrupt cached provider prefix cannot be accepted just because the
        # bar cursor matches; reconstruct and retain the full replay oracle.
        entry["providers"][next(iter(entry["providers"]))][0]["close"] = -1
        req = broker_request(list(range(100, 134)), state=actual["state"])
        before = incremental.COUNTERS["rebuilds"]
        actual = bot.decide(copy.deepcopy(req))
        self.assertEqual(incremental.COUNTERS["rebuilds"], before + 1)
        with patch.object(incremental, "evaluate", incremental.replay):
            self.assertEqual(actual, bot.decide(req))


class PythonSmaTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("python_sma", ROOT / "bot-plugins/python-sma/bot.py")
        cls.module = importlib.util.module_from_spec(spec);spec.loader.exec_module(cls.module)

    def test_shared_public_signal_keeps_private_accounts_and_recovery_independent(self):
        module = self.module;module.averages.cache_clear()
        req = request([100, 101, 102, 103, 104, 105, 106, 107])
        req.update(plugin_id="python.sma", plugin_version="1.0.0")
        req["config"] = dict(bar_interval_ms=1000, fast=3, slow=8, qty=1, inventory_cap=10, slippage_ticks=2)
        first = module.decide(req)
        self.assertEqual(first["actions"][0]["PlaceImmediateOrCancel"]["side"], "Buy")
        other = copy.deepcopy(req);other["participant"]["participant_id"] = "another"
        other["observation"]["own_account"]["Spot"]["available_cash"] = 0
        self.assertEqual(module.decide(other)["actions"], [])
        self.assertEqual(module.averages.cache_info().hits, 1)
        req["state"] = json.loads(json.dumps(first["state"]))
        self.assertEqual(module.decide(req)["actions"], [])
        req["observation"]["bot_market_data"]["candles"][0]["close_tick"] = 99
        with self.assertRaisesRegex(ValueError, "committed history"):
            module.decide(req)

    def test_tick_and_lot_precision_clip_the_real_order(self):
        req = request(list(range(100, 140, 5)))
        req.update(plugin_id="python.sma", plugin_version="1.0.0")
        req["config"] = dict(bar_interval_ms=1000, fast=3, slow=8, qty=3, inventory_cap=10, slippage_ticks=1)
        req["observation"]["instrument"] = dict(tick_size=5, lot_size=2)
        req["observation"]["book"]["asks"][0]["price_tick"] = 140
        action = self.module.decide(req)["actions"][0]["PlaceImmediateOrCancel"]
        self.assertEqual(action, dict(side="Buy", price_tick=140, qty=2))


class WorkerTransportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        spec = importlib.util.spec_from_file_location("pine_transport", ROOT / "bot-plugins/pine-strategy/transport.py")
        cls.module = importlib.util.module_from_spec(spec);spec.loader.exec_module(cls.module)

    def test_readonly_pine_consumer_leaves_cached_host_input_unchanged_after_success_and_failure(self):
        decoder = self.module.RequestDecoder();key = "b" * 64
        initial = broker_request([100, 101, 99, 103, 104, 102, 106, 107, 108])
        req = decoder.decode_readonly({"id":1, "cache_key":key, "request":initial})
        frozen = copy.deepcopy(req)
        expected = bot.decide(req)
        self.assertEqual(req, frozen)
        # Rejected speculative output cannot be inferred from the cached input.
        again = decoder.decode_readonly({"id":2, "cache_key":key, "base":1, "request_delta":[]})
        self.assertEqual(again, frozen)
        self.assertEqual(bot.decide(again), expected)
        invalid = copy.deepcopy(frozen);invalid["config"]["script"] = "missing-script.pine"
        req = decoder.decode_readonly({"id":3, "cache_key":key, "request":invalid})
        with self.assertRaises((ValueError, FileNotFoundError)):
            bot.decide(req)
        self.assertEqual(req, invalid)
        restored = decoder.decode_readonly({"id":4, "cache_key":key, "request":copy.deepcopy(frozen)})
        self.assertEqual(bot.decide(restored), expected)

    def test_incremental_tail_cache_remains_independent_of_consumer_input(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        state = None
        for end in range(1, 10):
            req = broker_request([100 + i for i in range(end)]);req["state"] = state
            before = copy.deepcopy(req)
            result = bot.decide(req)
            self.assertEqual(req, before)
            entry = next(iter(incremental.SESSIONS.values()))
            cached = copy.deepcopy({k:v for k,v in entry.items() if k != "session"})
            req["observation"]["bot_market_data"]["candles"][0]["close_tick"] = 999
            if req["state"]:
                req["state"]["accounts"][0]["position_size"] = 999
            self.assertEqual({k:v for k,v in entry.items() if k != "session"}, cached)
            state = result["state"]

    def test_exact_input_append_edit_remove_and_speculative_output_isolation(self):
        decoder = self.module.RequestDecoder();key = "a" * 64
        initial = {"state":{"bars":[1,2], "pending":"old"}, "observation":{"account":10}}
        decoded = decoder.decode({"id":1,"cache_key":key,"request":copy.deepcopy(initial)})
        decoded["state"]["bars"].append(999)
        delta = {"id":2,"cache_key":key,"base":1,"request_delta":[
            {"op":"append","path":["state","bars"],"from":2,"values":[3]},
            {"op":"remove","path":["state","pending"]},
            {"op":"set","path":["observation","account"],"value":11}]}
        self.assertEqual(decoder.decode(delta), {"state":{"bars":[1,2,3]},"observation":{"account":11}})
        # Host can reject a speculative decision and restore an older durable
        # input. Transport diffs must support truncation/replacement too.
        self.assertEqual(decoder.decode({"id":3,"cache_key":key,"base":2,"request_delta":[
            {"op":"set","path":["state"],"value":initial["state"]}]}),
            {"state":initial["state"],"observation":{"account":11}})
        with self.assertRaisesRegex(ValueError, "base mismatch"):
            decoder.decode({"id":4,"cache_key":key,"base":1,"request_delta":[]})

    def test_bad_append_eviction_and_full_request_recovery(self):
        decoder = self.module.RequestDecoder();key = "a" * 64
        decoder.decode({"id":1,"cache_key":key,"request":{"state":{"bars":[1]}}})
        with self.assertRaisesRegex(ValueError, "append boundary"):
            decoder.decode({"id":2,"cache_key":key,"base":1,"request_delta":[
                {"op":"append","path":["state","bars"],"from":2,"values":[3]}]})
        for i in range(self.module.MAX_REQUESTS + 1):
            decoder.decode({"id":i+10,"cache_key":f"{i:064x}","request":{"state":i}})
        self.assertEqual(len(decoder.requests), self.module.MAX_REQUESTS)
        with self.assertRaisesRegex(ValueError, "base mismatch"):
            decoder.decode({"id":1000,"cache_key":f"{0:064x}","base":10,"request_delta":[]})
        self.assertEqual(decoder.decode({"id":1001,"cache_key":key,"request":{"state":42}}), {"state":42})


if __name__ == "__main__": unittest.main()
