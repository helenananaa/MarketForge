"""Receipt integrity: no batch-return, rejection or missing event can fake admission."""
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from bot_runtime_receipts import audit_receipts, runtime_delta, runtime_metrics


def event(stage, wall_us, **values):
    return {"schema_version": 1, "stage": stage, "wall_us": wall_us, **values}


def decision(identifier=1, wall_us=150_000, count=1, close=1000, **progress):
    return event("decision", wall_us, decision_id=identifier, room_id="room",
                 participant_id="pine", instrument_id="spot", bar_close_ms=close, action_count=count, **progress)


class DurableReceiptTests(unittest.TestCase):
    def audit(self, events, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.jsonl"
            path.write_text("".join(json.dumps(row) + "\n" for row in events), encoding="utf-8")
            return audit_receipts(path, **kwargs)

    def clock(self):
        return event("clock_visible", 100_000, room_id="room", previous_market_ms=0, market_time_ms=1000)

    def test_only_actual_accepted_durable_action_contributes_to_accepted_latency(self):
        report = self.audit([self.clock(), decision(count=2),
            event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=2, outcome="accepted"),
            event("action_durable", 900_000, decision_id=1, action_index=1, command_seq=3, outcome="rejected")])
        self.assertTrue(report["qualified"])
        self.assertEqual(report["accepted_bar_close_to_durable"]["p95_ms"], 100)
        self.assertEqual(report["rejected_bar_close_to_durable"]["p95_ms"], 800)
        self.assertEqual(report["whole_run_action_dispositions"], {"accepted": 1, "rejected": 1})

    def test_batch_return_does_not_prove_any_action_was_admitted(self):
        report = self.audit([self.clock(), decision(), event("commit_return", 200_000, decision_id=1)])
        self.assertFalse(report["qualified"])
        self.assertEqual(report["whole_run_action_dispositions"], {"pending": 1})

    def test_no_order_commit_and_discard_are_distinct_from_accepted_orders(self):
        report = self.audit([self.clock(), decision(count=0),
            event("decision_state_committed", 200_000, decision_id=1), decision(2),
            event("decision_discarded", 220_000, decision_id=2, reason="stale")])
        self.assertFalse(report["qualified"])
        self.assertEqual(report["no_order_bar_close_to_state_durable"]["p95_ms"], 100)
        self.assertEqual(report["whole_run_action_dispositions"], {"discarded": 1})

    def test_rejected_only_cannot_pass_accepted_latency_requirement(self):
        report = self.audit([self.clock(), decision(),
            event("action_durable", 160_000, decision_id=1, action_index=0, command_seq=1, outcome="rejected")])
        self.assertFalse(report["accepted_latency_exercised"])
        self.assertFalse(report["qualified"])

    def test_prewarm_and_account_wakes_are_excluded_without_fabricated_anchors(self):
        report = self.audit([event("clock_visible", 100_000, room_id="room", previous_market_ms=1000, market_time_ms=2000),
            decision(close=500), decision(2, close=None),
            event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted"),
            event("action_durable", 200_000, decision_id=2, action_index=0, command_seq=2, outcome="accepted")])
        self.assertEqual(report["excluded"], {"prewarm_bar_decisions": 1, "non_bar_decisions": 1})
        self.assertFalse(report["errors"])
        self.assertFalse(report["qualified"])

    def test_missing_runtime_bar_anchor_cannot_be_excluded_into_a_pass(self):
        report = self.audit([self.clock(), decision(),
            event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted"),
            event("clock_visible", 300_000, room_id="room", previous_market_ms=2000, market_time_ms=3000),
            decision(2, wall_us=350_000, close=2000),
            event("action_durable", 400_000, decision_id=2, action_index=0, command_seq=2, outcome="accepted")])
        self.assertTrue(report["accepted_latency_exercised"])
        self.assertEqual(report["excluded"], {"unanchored_bar_decisions": 1})
        self.assertIn("new closed-bar decision has no clock publication anchor", report["errors"])
        self.assertFalse(report["qualified"])

    def test_failed_discarded_or_pending_state_only_bar_decisions_cannot_pass(self):
        for terminal in ("decision_failed", "decision_discarded", "pending"):
            with self.subTest(terminal=terminal):
                events = [self.clock(), decision(),
                    event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted"),
                    decision(2, count=0)]
                if terminal != "pending":
                    events.append(event(terminal, 220_000, decision_id=2))
                report = self.audit(events)
                self.assertTrue(report["accepted_latency_exercised"])
                self.assertEqual(report["bar_cohort_no_order_dispositions"], {terminal: 1})
                self.assertFalse(report["qualified"])

    def test_state_only_discard_outside_bar_cohort_is_retained_but_not_reclassified(self):
        report = self.audit([self.clock(), decision(1, count=0),
            event("decision_discarded", 160_000, decision_id=1),
            event("clock_visible", 300_000, room_id="room", previous_market_ms=1000, market_time_ms=2000),
            decision(2, wall_us=350_000, close=2000),
            event("action_durable", 400_000, decision_id=2, action_index=0, command_seq=1, outcome="accepted")],
            start_wall_us=200_000)
        self.assertTrue(report["qualified"])
        self.assertEqual(report["no_order_decisions"], {"decision_discarded": 1})
        self.assertEqual(report["bar_cohort_no_order_dispositions"], {})

    def test_cutoff_progress_requires_complete_durable_commit_and_keeps_drain_separate(self):
        def cumulative(identifier, wall, count, bars, close=None):
            return decision(identifier, wall, count, close, evaluated_bars=bars,
                            last_closed_bar_ms=bars * 1000, bar_interval_ms=1000)
        report = self.audit([self.clock(), cumulative(1, 150_000, 0, 1, 1000),
            event("decision_state_committed", 180_000, decision_id=1),
            # Complete mixed accepted/rejected actions durably advance the state.
            cumulative(2, 200_000, 2, 1),
            event("action_durable", 220_000, decision_id=2, action_index=0, command_seq=1, outcome="accepted"),
            event("action_durable", 240_000, decision_id=2, action_index=1, command_seq=2, outcome="rejected"),
            event("clock_visible", 250_000, room_id="room", previous_market_ms=1000, market_time_ms=2000),
            cumulative(3, 280_000, 2, 2, 2000),
            event("action_durable", 290_000, decision_id=3, action_index=0, command_seq=3, outcome="accepted"),
            event("action_durable", 350_000, decision_id=3, action_index=1, command_seq=4, outcome="accepted"),
            event("clock_visible", 360_000, room_id="room", previous_market_ms=2000, market_time_ms=3000),
            cumulative(4, 380_000, 0, 3, 3000),
            event("decision_state_committed", 400_000, decision_id=4)], end_wall_us=300_000)
        self.assertTrue(report["qualified"])
        self.assertEqual(report["cutoff_market_by_room"], {"room": 2000})
        self.assertEqual(report["at_cutoff_progress"]["room"]["pine"], {
            "evaluated_bars": 1, "last_closed_bar_ms": 1000, "bar_interval_ms": 1000,
            "committed_wall_us": 240_000, "decision_id": 2})
        self.assertEqual(report["bar_cohort_action_dispositions"], {"accepted": 2})
        self.assertEqual(report["accepted_bar_close_to_durable"]["max_ms"], 100)

    def test_cutoff_progress_uses_commit_order_and_includes_account_wakes(self):
        early = decision(1, 120_000, 0, 1000, evaluated_bars=1, last_closed_bar_ms=1000, bar_interval_ms=1000)
        later = decision(2, 200_000, 0, None, evaluated_bars=2, last_closed_bar_ms=2000, bar_interval_ms=1000)
        report = self.audit([self.clock(), later, event("decision_state_committed", 240_000, decision_id=2),
            early, event("decision_state_committed", 160_000, decision_id=1)], end_wall_us=300_000)
        self.assertEqual(report["at_cutoff_progress"]["room"]["pine"]["decision_id"], 2)
        self.assertEqual(report["at_cutoff_progress"]["room"]["pine"]["evaluated_bars"], 2)

    def test_failed_partial_and_missing_progress_never_fabricate_cutoff_completion(self):
        for outcome in ("decision_failed", "decision_discarded", "pending", "partial"):
            with self.subTest(outcome=outcome):
                events = [self.clock(), decision(count=2 if outcome == "partial" else 0,
                    evaluated_bars=1, last_closed_bar_ms=1000, bar_interval_ms=1000)]
                if outcome == "partial":
                    events.append(event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted"))
                elif outcome != "pending":
                    events.append(event(outcome, 200_000, decision_id=1))
                report = self.audit(events, end_wall_us=250_000)
                self.assertEqual(report["at_cutoff_progress"], {})
        missing = self.audit([self.clock(), decision(count=0),
            event("decision_state_committed", 200_000, decision_id=1)], end_wall_us=250_000)
        self.assertEqual(missing["at_cutoff_progress"], {})
        partial = self.audit([self.clock(), decision(count=0, evaluated_bars=1),
            event("decision_state_committed", 200_000, decision_id=1)], end_wall_us=250_000)
        self.assertIn("invalid cumulative decision progress", partial["errors"])

    def test_without_cutoff_no_final_progress_is_mislabeled_as_cutoff_progress(self):
        report = self.audit([self.clock(), decision(count=0, evaluated_bars=1,
            last_closed_bar_ms=1000, bar_interval_ms=1000), event("decision_state_committed", 200_000, decision_id=1)])
        self.assertFalse(report["cutoff_available"])
        self.assertEqual(report["at_cutoff_progress"], {})
        self.assertEqual(report["cutoff_market_by_room"], {})

    def test_window_is_selected_by_bar_close_not_late_decision_completion(self):
        report = self.audit([self.clock(), decision(wall_us=500_000),
            event("action_durable", 600_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted")],
            start_wall_us=200_000)
        self.assertEqual(report["excluded"], {"outside_window_bar_decisions": 1})
        self.assertFalse(report["accepted_latency_exercised"])

    def test_missing_no_order_terminal_prevents_complete_outcome_claim(self):
        report = self.audit([self.clock(), decision(count=0), decision(2),
            event("action_durable", 200_000, decision_id=2, action_index=0, command_seq=1, outcome="accepted")])
        self.assertTrue(report["complete_generated_action_receipts"])
        self.assertFalse(report["complete_decision_outcomes"])
        self.assertFalse(report["qualified"])

    def test_conflicting_or_duplicate_receipts_fail_closed(self):
        admitted = event("action_durable", 200_000, decision_id=1, action_index=0, command_seq=1, outcome="accepted")
        report = self.audit([self.clock(), decision(), admitted, admitted,
                            event("decision_failed", 220_000, decision_id=1)])
        self.assertFalse(report["qualified"])
        self.assertTrue(report["errors"])

    def test_actual_runtime_metrics_retain_fallback_reasons_and_window_deltas(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "metrics"
            path.write_text('marketforge_bot_runtime_diagnostics 1\n'
                            'marketforge_bot_runtime_route_total{route="process",reason="source_unqualified"} 3\n'
                            'marketforge_bot_runtime_evaluation_total{route="process",mode="replay",reason="source_unqualified"} 3\n', encoding="utf-8")
            before = runtime_metrics(path)
            path.write_text(path.read_text().replace('} 3', '} 8'), encoding="utf-8")
            delta = runtime_delta(runtime_metrics(path), before)
        self.assertEqual(delta["routes"][0]["count"], 5)
        self.assertEqual(delta["evaluations"][0]["mode"], "replay")
        self.assertFalse(delta["counter_reset"])


def load_module(name, relative):
    spec = importlib.util.spec_from_file_location(name, ROOT / relative)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class WorkerDiagnosticsTests(unittest.TestCase):
    def test_evaluation_mode_is_reported_at_actual_append_rebuild_or_replay(self):
        incremental = load_module("diagnostic_incremental", "bot-plugins/pine-strategy/incremental.py")
        class Program:
            def external_session(self, **kwargs):
                return SimpleNamespace(append=lambda *args, **kwargs: {"intents": [], "intent_count": 0})
            def run_external(self, *args, **kwargs): return "replay"
        request = {"participant": "one", "seed": 7, "config": {}, "state": {}}
        program = Program()
        with patch.object(incremental, "qualified", return_value=True):
            incremental.evaluate(program, "source", request, [1], [1], execution_passes=[{}])
            self.assertEqual(incremental.LAST_EVALUATION, {"mode": "rebuild", "reason": "session_missing"})
            request["state"] = {"bars": [1]}
            incremental.evaluate(program, "source", request, [1, 2], [1, 2], execution_passes=[{}, {}])
            self.assertEqual(incremental.LAST_EVALUATION, {"mode": "append", "reason": None})
            incremental.evaluate(program, "source", request, [9, 2], [1, 2], execution_passes=[{}, {}])
            self.assertEqual(incremental.LAST_EVALUATION, {"mode": "rebuild", "reason": "state_mismatch"})
        with patch.object(incremental, "qualified", return_value=False):
            incremental.evaluate(program, "source", request, [1], [1], execution_passes=[{}])
            self.assertEqual(incremental.LAST_EVALUATION, {"mode": "replay", "reason": "source_unqualified"})

    def test_worker_metadata_requires_host_opt_in_and_is_not_strategy_state(self):
        bot = load_module("diagnostic_bot", "bot-plugins/pine-strategy/bot.py")
        request = {"config": {"execution_mode": "broker"}}
        envelope = {"protocol": "bot.worker.v1", "id": 1, "request": request}
        response = {"actions": [], "state": {"untouched": 7}}
        for enabled in (False, True):
            output = io.StringIO()
            with patch.dict(os.environ, {"MARKETFORGE_BOT_WORKER_DIAGNOSTICS": "1" if enabled else "0"}, clear=True), \
                 patch.object(sys, "stdin", io.StringIO(json.dumps(envelope) + "\n")), \
                 patch.object(sys, "stdout", output), patch.object(bot, "decide", return_value=response):
                bot.serve_worker()
            value = json.loads(output.getvalue())
            self.assertEqual(value["response"], response)
            self.assertEqual("runtime_profile" in value, enabled)
            if enabled: self.assertEqual(value["runtime_profile"]["mode"], "no_evaluation")

    def test_reused_validated_encoding_preserves_optional_profile(self):
        bot = load_module("diagnostic_bot_encode", "bot-plugins/pine-strategy/bot.py")
        value = {"protocol": "bot.worker.v1", "id": 1, "response": {"state": {}},
                 "runtime_profile": {"mode": "append", "reason": None}}
        encoded = bot.encode_worker_response(value, [json.dumps(value["response"])])
        self.assertEqual(json.loads(encoded), value)

    def test_optional_metadata_cannot_invalidate_near_limit_strategy_response(self):
        bot = load_module("diagnostic_bot_limit", "bot-plugins/pine-strategy/bot.py")
        original = {"protocol": "bot.worker.v1", "id": 1, "response": {"padding": ""}}
        overhead = len(json.dumps(original, separators=(",", ":")))
        original["response"]["padding"] = "x" * (1_048_576 - overhead - 1)
        profiled = {**original, "runtime_profile": {"mode": "append", "reason": None}}
        encoded = bot.encode_worker_response(profiled, [])
        self.assertEqual(len(encoded) + 1, 1_048_576)
        self.assertEqual(json.loads(encoded), original)


class SustainedAcceptedCoverageTests(unittest.TestCase):
    sources = ("ma.pine", "rsi.pine", "breakout.pine")

    def audit(self, samples, **kwargs):
        rows, clocks = [], {}
        for identifier, (anchor_second, source, latency_ms, outcome) in enumerate(sorted(samples), 1):
            anchor = int(anchor_second * 1_000_000)
            if anchor not in clocks:
                previous = len(clocks) * 1000
                clocks[anchor] = previous + 1000
                rows.append(event("clock_visible", anchor, room_id="room",
                    previous_market_ms=previous, market_time_ms=previous+1000))
            rows.append(event("decision", anchor+1_000, decision_id=identifier,
                room_id="room", participant_id=source, bar_close_ms=clocks[anchor], action_count=1))
            rows.append(event("action_durable", anchor+latency_ms*1000, decision_id=identifier,
                action_index=0, command_seq=identifier, outcome=outcome))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "trace.jsonl"
            path.write_text("".join(json.dumps(row)+"\n" for row in rows), encoding="utf-8")
            return audit_receipts(path, start_wall_us=0,
                participant_sources={"room":{source:source for source in self.sources}},
                **{"end_wall_us":120_000_000, "require_sustained_coverage":True, **kwargs})

    def wave(self, at, count=20, latency=100):
        return [(at, self.sources[index % 3], latency, "accepted") for index in range(count)]

    def test_each_minute_requires_exercised_accepted_actions_from_all_three_sources(self):
        report = self.audit(self.wave(20)+self.wave(80))
        self.assertTrue(report["qualified"])
        coverage = report["accepted_window_coverage"]
        self.assertTrue(coverage["qualified"])
        self.assertEqual([row["accepted_actions"] for row in coverage["windows"]], [20, 20])
        self.assertEqual(coverage["maximum_accepted_anchor_gap_seconds"], 60)

    def test_many_rejections_cannot_supply_accepted_coverage(self):
        samples = self.wave(20)+self.wave(80, 19)
        samples += [(80, "ma.pine", 100, "rejected")] * 100
        report = self.audit(samples)
        self.assertFalse(report["qualified"])
        window = report["accepted_window_coverage"]["windows"][1]
        self.assertEqual(window["accepted_actions"], 19)
        self.assertEqual(window["action_dispositions"]["rejected"], 100)

    def test_missing_one_source_cannot_hide_behind_large_total(self):
        report = self.audit(self.wave(20)+[(80, "ma.pine", 100, "accepted")] * 200)
        self.assertFalse(report["qualified"])
        self.assertFalse(report["accepted_window_coverage"]["windows"][1]["meets_coverage"])

    def test_empty_tail_and_cross_window_anchor_gap_cannot_pass(self):
        for samples, expected_gap in ((self.wave(1)+self.wave(119), 118),
                                      (self.wave(1, 1000), 119)):
            with self.subTest(gap=expected_gap):
                report = self.audit(samples)
                self.assertFalse(report["qualified"])
                self.assertEqual(report["accepted_window_coverage"]["maximum_accepted_anchor_gap_seconds"], expected_gap)

    def test_late_window_latency_failure_cannot_hide_in_pooled_percentile(self):
        report = self.audit(self.wave(20, 1000)+self.wave(80, latency=500))
        self.assertEqual(report["accepted_bar_close_to_durable"]["p95_ms"], 100)
        self.assertEqual(report["accepted_window_coverage"]["windows"][1]["accepted_bar_close_to_durable"]["p95_ms"], 500)
        self.assertFalse(report["qualified"])

    def test_cohort_uses_anchor_not_terminal_time_and_boundary_is_counted_once(self):
        samples = self.wave(59.9, latency=200)+self.wave(60, 1)+self.wave(100, 19)
        report = self.audit(samples)
        self.assertTrue(report["qualified"])
        self.assertEqual([row["accepted_actions"] for row in report["accepted_window_coverage"]["windows"]], [20, 20])

    def test_small_final_remainder_merges_without_excluding_late_actions(self):
        report = self.audit(self.wave(20)+self.wave(80)+self.wave(123, 1), end_wall_us=124_000_000)
        windows = report["accepted_window_coverage"]["windows"]
        self.assertTrue(report["qualified"])
        self.assertEqual(len(windows), 2)
        self.assertEqual(windows[1]["to_second"], 124)
        self.assertEqual(windows[1]["accepted_actions"], 21)

    def test_short_optional_smoke_retains_coverage_without_requiring_twenty_actions(self):
        report = self.audit(self.wave(5, 1), end_wall_us=10_000_000, require_sustained_coverage=False)
        self.assertTrue(report["qualified"])
        self.assertIsNone(report["accepted_window_coverage"]["qualified"])
        self.assertFalse(report["accepted_window_coverage"]["required"])


if __name__ == "__main__": unittest.main()
