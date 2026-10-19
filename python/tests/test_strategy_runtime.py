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
        # Library entries are qualified by scripts/qualify_library_incremental.py.
        cases = [script for script in json.loads((ROOT / "bot-plugins/pine-strategy/incremental-qualified.json").read_text())["source_sha256"]
                 if not script.startswith("library/")]
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
                    frozen = copy.deepcopy(req)
                    encoded = []
                    borrowed = bot.decide(req, borrow_history=True, encoded_response=encoded)
                    self.assertEqual(borrowed, fast, (script, seed, end))
                    self.assertEqual(json.loads(encoded[0]), fast)
                    self.assertEqual(req, frozen)
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

    def test_worker_borrowed_history_matches_owned_state_through_append_quiet_and_rollback(self):
        state = None
        requests = []
        for end in (9, 10, 10, 11):
            req = broker_request([100 + i for i in range(end)])
            req["state"] = state
            requests.append(copy.deepcopy(req))
            frozen = copy.deepcopy(req)
            expected = bot.decide(req)
            encoded = []
            actual = bot.decide(req, borrow_history=True, encoded_response=encoded)
            self.assertEqual(actual, expected)
            self.assertEqual(json.loads(encoded[0]), expected)
            self.assertEqual(req, frozen)
            # The response is serialized before the next decoder call. Owned
            # public calls still have independently mutable returned history.
            if req["state"]:
                expected["state"]["accounts"][0]["position_size"] = 999
                self.assertEqual(req, frozen)
            state = json.loads(encoded[0])["state"]
        for req in (requests[1], requests[0]):
            frozen = copy.deepcopy(req)
            bot.incremental_module().SESSIONS.clear()
            expected = bot.decide(req)
            actual = bot.decide(req, borrow_history=True)
            self.assertEqual(actual, expected)
            self.assertEqual(req, frozen)
        invalid = copy.deepcopy(requests[-1])
        invalid["state"]["bars"][0]["close"] += 1
        frozen = copy.deepcopy(invalid)
        with self.assertRaisesRegex(ValueError, "history changed"):
            bot.decide(invalid, borrow_history=True)
        self.assertEqual(invalid, frozen)

    def test_worker_encoded_response_reuse_preserves_json_and_framing_boundary(self):
        for value in ({"unicode": "汉字", "escaped": '\\"\n', "nested": [1, -0.0, {}]},
                      [0] * 349520):
            response = {"protocol": "bot.worker.v1", "id": 123, "response": value}
            validated = json.dumps(value, allow_nan=False)
            encoded = bot.encode_worker_response(response, [validated])
            self.assertEqual(json.loads(encoded), response)
            ordinary = json.dumps(response, separators=(",", ":"), allow_nan=False)
            if len(validated.encode()) + 50 > 1_048_576:
                self.assertEqual(encoded, ordinary)
            else:
                self.assertIn(validated, encoded)

    def test_incremental_tail_cache_remains_independent_of_consumer_input(self):
        incremental = bot.incremental_module();incremental.SESSIONS.clear()
        state = None
        for end in range(1, 10):
            req = broker_request([100 + i for i in range(end)]);req["state"] = state
            before = copy.deepcopy(req)
            result = bot.decide(req)
            self.assertEqual(req, before)
            entry = next(iter(incremental.SESSIONS.values()))
            history = lambda: {k:v for k,v in entry.items() if k not in ("session", "digest")}
            cached = copy.deepcopy(history())
            req["observation"]["bot_market_data"]["candles"][0]["close_tick"] = 999
            if req["state"]:
                req["state"]["accounts"][0]["position_size"] = 999
            self.assertEqual(history(), cached)
            state = result["state"]

    def test_patches_copy_on_write_so_seen_objects_never_change(self):
        decoder = self.module.RequestDecoder();key = "c" * 64
        first = decoder.decode_readonly({"id":1, "cache_key":key, "request":{"candles":[{"t":1}], "state":{"bars":[1]}}})
        candles, bar = first["candles"], first["candles"][0]
        second = decoder.decode_readonly({"id":2, "cache_key":key, "base":1, "request_delta":[
            {"op":"append","path":["candles"],"from":1,"values":[{"t":2}]},
            {"op":"set","path":["candles",0,"t"],"value":9}]})
        self.assertEqual(first, {"candles":[{"t":1}], "state":{"bars":[1]}})
        self.assertIs(first["candles"], candles);self.assertIs(candles[0], bar)
        self.assertEqual(second["candles"], [{"t":9}, {"t":2}])
        self.assertIsNot(second["candles"][0], bar);self.assertIs(second["state"], first["state"])
        decoder.fold_state(key, [{"op":"append","path":["bars"],"from":1,"values":[2]}])
        self.assertEqual(second["state"], {"bars":[1]})
        third = decoder.decode_readonly({"id":3, "cache_key":key, "base":2, "request_delta":[]})
        self.assertEqual(third["state"], {"bars":[1, 2]})

    def test_state_delta_is_exact_type_strict_and_measures_the_full_encoding(self):
        rng = random.Random(3)
        leaves = [0, 1, 1.0, -0.0, 0.0, True, None, "x", "汉\"\n", 2**40]
        def value(depth=0):
            if depth > 2 or rng.random() < 0.4: return rng.choice(leaves)
            if rng.random() < 0.5: return [value(depth + 1) for _ in range(rng.randrange(4))]
            return {rng.choice("abc"): value(depth + 1) for _ in range(rng.randrange(4))}
        for _ in range(500):
            old = {k: value() for k in rng.sample("abcdef", rng.randrange(6))}
            old["history"] = [value() for _ in range(rng.randrange(4))]
            new = copy.deepcopy(old)
            new.update({k: value() for k in rng.sample("abcdef", rng.randrange(3))})
            for k in rng.sample(sorted(new), min(len(new), rng.randrange(2))): del new[k]
            if "history" in new: new["history"] = old["history"] + [value() for _ in range(rng.randrange(3))]
            patches, change = self.module.state_delta(old, new)
            self.assertEqual(len(json.dumps(old)) + change, len(json.dumps(new)))
            applied = self.module.apply_patches({"state": json.loads(json.dumps(old))},
                                                json.loads(json.dumps(patches)), ("state",))["state"]
            self.assertTrue(self.module.same(applied, json.loads(json.dumps(new))), (old, new, patches))
        self.assertEqual(self.module.state_delta({"x":1, "z":0.0}, {"x":1.0, "z":-0.0})[0],
                         [{"op":"set","path":["x"],"value":1.0}, {"op":"set","path":["z"],"value":-0.0}])
        for bad in ({1: 2}, {"x": (1, 2)}, {"x": float("inf")}):
            with self.assertRaises(self.module.NotDelta):
                self.module.state_delta({}, bad)

    def test_state_delta_replies_rebuild_the_full_state_and_reuse_parses_exactly(self):
        decoder = self.module.RequestDecoder();lengths = self.module.StateLengths();key = "d" * 64
        host = None  # the host's cached request, folded as the worker folds
        state = None
        prices = [100, 101, 99, 103, 104, 102, 106, 107, 108, 105, 109, 111]
        for end in range(9, len(prices) + 1):
            req = broker_request(prices[:end]);req["state"] = copy.deepcopy(state)
            changed = end == 11
            if changed:
                # A changed committed candle is parsed again, so the change is refused.
                req["observation"]["bot_market_data"]["candles"][3]["volume"] += 1
            wire = json.loads(json.dumps(req))
            if host is None:
                envelope = {"id":end, "cache_key":key, "request":wire}
            else:
                patches = []
                self._host_diff(host[1], wire, [], patches)
                envelope = {"id":end, "cache_key":key, "base":host[0], "request_delta":json.loads(json.dumps(patches))}
            decoded = decoder.decode_readonly(envelope)
            if changed:
                for attempt in (lambda: bot.decide(decoded, borrow_history=True, defer_state=True, immutable_input=True),
                                lambda: bot.decide(json.loads(json.dumps(req)))):
                    with self.assertRaisesRegex(ValueError, "history changed"):
                        attempt()
                # A failed worker is replaced; its successor starts from full requests.
                decoder, lengths, host = self.module.RequestDecoder(), self.module.StateLengths(), None
                continue
            reply = {"protocol":"bot.worker.v1", "id":end}
            encoded = []
            reply["response"] = bot.decide(decoded, borrow_history=True, encoded_response=encoded,
                                           defer_state=True, immutable_input=True)
            self.assertEqual(encoded, [])
            fold = bot.state_reply(self.module, lengths, key, decoded["state"], reply, encoded)
            bot.incremental_module().SESSIONS.clear()
            expected = bot.decide(json.loads(json.dumps(req)))
            if fold is None:
                self.assertIn(end, (9,));self.assertEqual(json.loads(encoded[0]), expected)
                state, host = json.loads(encoded[0])["state"], (end, wire)
                continue
            sent = json.loads(fold[0])
            self.assertIsNone(sent["response"]["state"])
            self.assertEqual(sent["response"]["actions"], expected["actions"])
            self.assertEqual(fold[2], len(json.dumps(expected["state"])))
            prefixed = [dict(p, path=["state", *p["path"]]) for p in sent["state_delta"]]
            state = self.module.apply_patches({"state": wire["state"]}, prefixed)["state"]
            self.assertTrue(self.module.same(state, json.loads(json.dumps(expected["state"]))), end)
            decoder.fold_state(key, fold[1]);lengths.remember(key, decoder.requests[key][1]["state"], fold[2])
            host = (end, self.module.apply_patches(wire, prefixed))
            state = json.loads(json.dumps(state))

    def _host_diff(self, old, new, path, out):
        same = self.module.same
        if same(old, new): return
        if type(old) is dict and type(new) is dict:
            out += [{"op":"remove","path":path + [k]} for k in old if k not in new]
            for k, v in new.items():
                if k in old: self._host_diff(old[k], v, path + [k], out)
                else: out.append({"op":"set","path":path + [k],"value":v})
        elif type(old) is list and type(new) is list and len(new) >= len(old) and all(map(same, old, new)):
            out.append({"op":"append","path":path,"from":len(old),"values":new[len(old):]})
        else:
            out.append({"op":"set","path":path,"value":new})

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
