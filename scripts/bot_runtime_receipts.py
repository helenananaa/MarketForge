"""Audit actual runtime routes and per-action durable receipts without inferring success.

Clock-visible events provide wall-clock anchors for newly closed bars. Prewarmed
bars have no anchor and never acquire a fabricated latency. Rejected executions
are durable facts, but are excluded from accepted-order latency and throughput.
"""
from collections import Counter, defaultdict
import argparse
import bisect
import json
import math
from pathlib import Path
import re


def runtime_metrics(path):
    result = {"present": False, "routes": [], "evaluations": [], "trace_enabled": False,
              "trace_errors": 0, "wall_us": None}
    if not Path(path).is_file():
        return result
    pattern = re.compile(r'^marketforge_bot_runtime_(route|evaluation)_total\{([^}]+)\} (\d+)$')
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        match = pattern.fullmatch(line)
        if match:
            labels = dict(re.findall(r'(\w+)="([a-z_]+)"', match[2]))
            result["routes" if match[1] == "route" else "evaluations"].append(
                {**labels, "count": int(match[3])})
        elif line.startswith("marketforge_bot_"):
            name, _, value = line.partition(" ")
            if name == "marketforge_bot_runtime_diagnostics": result["present"] = value == "1"
            elif name == "marketforge_bot_trace_enabled": result["trace_enabled"] = value == "1"
            elif name == "marketforge_bot_trace_errors_total": result["trace_errors"] = int(value)
            elif name == "marketforge_bot_trace_wall_us": result["wall_us"] = int(value)
    return result


def runtime_delta(after, before):
    """A warmup snapshot is required to claim a measurement-window count."""
    result = {"present": after["present"], "measurement_window_available": before["present"],
              "trace_enabled": after["trace_enabled"], "trace_errors": after["trace_errors"],
              "scope": "difference between the supplied metric snapshots; benchmark snapshots exclude drain"}
    for field in ("routes", "evaluations"):
        old = {tuple(sorted((k, v) for k, v in row.items() if k != "count")): row["count"]
               for row in before[field]}
        result[field] = [{**row, "count": row["count"] - old.get(
            tuple(sorted((k, v) for k, v in row.items() if k != "count")), 0)} for row in after[field]]
    result["counter_reset"] = any(row["count"] < 0 for field in ("routes", "evaluations") for row in result[field])
    result["process_modes_source"] = "optional worker-reported evaluation mode; missing metadata is unknown"
    return result


def percentiles(values):
    values = sorted(values)
    return {"count": len(values), "p50_ms": values[max(0, math.ceil(len(values) * .5) - 1)] if values else None,
            "p95_ms": values[max(0, math.ceil(len(values) * .95) - 1)] if values else None,
            "max_ms": values[-1] if values else None}


def _integer(value):
    return type(value) is int and value >= 0


class AcceptedWindowCoverage:
    """Count accepted actions in bar-anchor cohorts, independently of drain time."""
    SOURCES = ("ma.pine", "rsi.pine", "breakout.pine")

    def __init__(self, start_wall_us, end_wall_us, participant_sources, *, required=False, max_p95_ms=250):
        self.start, self.end = start_wall_us, end_wall_us
        self.participant_sources = participant_sources or {}
        self.required, self.max_p95_ms = required, max_p95_ms
        self.starts, self.windows, self.accepted_anchors = [], [], set()
        if not (_integer(self.start) and _integer(self.end) and self.end > self.start):
            return
        self.starts = list(range(self.start, self.end, 60_000_000))
        if len(self.starts) > 1 and self.end - self.starts[-1] < 5_000_000:
            self.starts.pop()
        for index, start in enumerate(self.starts):
            end = self.starts[index+1] if index+1 < len(self.starts) else self.end
            self.windows.append({"start_wall_us":start, "end_wall_us":end,
                "from_second":(start-self.start)/1e6, "to_second":(end-self.start)/1e6,
                "dispositions":Counter(), "latencies":[], "sources":Counter(),
                "participants":set(), "unmapped_actions":0})

    def record(self, anchor, room, participant, outcome, latency_ms=None):
        if not self.starts or not self.start <= anchor <= self.end:
            return
        # Adjacent windows are half-open; the final cutoff itself belongs to the last window.
        window = self.windows[bisect.bisect_right(self.starts, anchor)-1]
        source = self.participant_sources.get(room, {}).get(participant)
        if source not in self.SOURCES:
            window["unmapped_actions"] += 1
            return
        window["dispositions"][outcome] += 1
        if outcome == "accepted":
            window["latencies"].append(latency_ms)
            window["sources"][source] += 1
            window["participants"].add((room, participant))
            self.accepted_anchors.add(anchor)

    def finish(self):
        windows = []
        for window in self.windows:
            latency = percentiles(window["latencies"])
            source_counts = {source:window["sources"][source] for source in self.SOURCES}
            windows.append({key:window[key] for key in
                ("start_wall_us", "end_wall_us", "from_second", "to_second", "unmapped_actions")})
            windows[-1].update(action_dispositions=dict(window["dispositions"]),
                accepted_actions=latency["count"], accepted_by_source=source_counts,
                accepted_participants=len(window["participants"]), accepted_bar_close_to_durable=latency,
                meets_coverage=(latency["count"] >= 20 and all(source_counts.values())
                    and latency["p95_ms"] <= self.max_p95_ms and window["unmapped_actions"] == 0))
        anchors = sorted(self.accepted_anchors)
        endpoints = [self.start, *anchors, self.end] if self.windows else []
        longest_gap = max((b-a for a,b in zip(endpoints, endpoints[1:])), default=None)
        meets = (bool(windows) and bool(anchors) and all(row["meets_coverage"] for row in windows)
                 and longest_gap <= 60_000_000)
        return {"required":self.required, "qualified":meets if self.required else None,
            "minimum_accepted_actions_per_window":20, "minimum_per_source_per_window":1,
            "required_sources":list(self.SOURCES), "accepted_p95_limit_ms":self.max_p95_ms,
            "maximum_accepted_anchor_gap_seconds":longest_gap/1e6 if longest_gap is not None else None,
            "accepted_anchor_gap_limit_seconds":60, "windows":windows,
            "scope":"60-second bar-close-anchor cohorts; final remainder under 5 seconds merges into the prior window; accepted durable outcomes may complete after the cohort; gap includes measurement endpoints; admissions do not prove fills"}


def audit_receipts(path, *, start_wall_us=None, end_wall_us=None, max_accepted_p95_ms=250,
                   participant_sources=None, require_sustained_coverage=False):
    """Cohort: decisions whose *bar-close anchor* lies in the measurement window.

    Terminal receipts after the window are retained for that cohort. Every
    generated action also has a whole-run disposition, including non-bar work.
    """
    result = {"available": Path(path).is_file(), "schema_version": 1, "errors": [],
              "window": {"start_wall_us": start_wall_us, "end_wall_us": end_wall_us},
              "scope": "bar-close-visible cohort; durable terminal receipts include drain; whole-run dispositions also retained",
              "latency_definition": "first successful clock publication crossing the actual new bar close to accepted action durable install",
              "threshold_ms": max_accepted_p95_ms,
              "cutoff_available": end_wall_us is not None,
              "cutoff_market_by_room": {}, "at_cutoff_progress": {}}
    coverage = AcceptedWindowCoverage(start_wall_us, end_wall_us, participant_sources,
        required=require_sustained_coverage, max_p95_ms=max_accepted_p95_ms)
    if not result["available"]:
        result["qualified"] = False
        result["accepted_window_coverage"] = coverage.finish()
        return result
    def events():
        with Path(path).open(encoding="utf-8") as stream:
            for line_number, line in enumerate(stream, 1):
                try:
                    event = json.loads(line)
                    if not isinstance(event, dict) or event.get("schema_version") != 1 or not _integer(event.get("wall_us")):
                        raise ValueError("invalid schema or monotonic timestamp")
                    yield event
                except (ValueError, TypeError) as error:
                    result["errors"].append(f"line {line_number}: {error}")
    clocks = defaultdict(list)
    decisions, terminals, actions = {}, {}, {}
    for event in events():
        stage = event.get("stage")
        identifier = event.get("decision_id")
        if stage == "clock_visible":
            previous, current = event.get("previous_market_ms"), event.get("market_time_ms")
            if not isinstance(event.get("room_id"), str) or not _integer(previous) or not _integer(current) or current <= previous:
                result["errors"].append("invalid clock-visible interval")
            else:
                clocks[event["room_id"]].append(event)
        elif stage == "decision":
            if not _integer(identifier) or identifier in decisions or not _integer(event.get("action_count")):
                result["errors"].append("invalid or duplicate decision identity")
            elif not isinstance(event.get("room_id"), str) or not isinstance(event.get("participant_id"), str):
                result["errors"].append("decision is missing room or participant identity")
            elif event.get("bar_close_ms") is not None and not _integer(event["bar_close_ms"]):
                result["errors"].append("invalid closed-bar timestamp")
            else:
                decisions[identifier] = event
        elif stage == "action_durable":
            key = (identifier, event.get("action_index"))
            if not _integer(identifier) or not _integer(key[1]) or key in actions or event.get("outcome") not in ("accepted", "rejected") or not _integer(event.get("command_seq")):
                result["errors"].append("invalid or duplicate durable action receipt")
            else:
                actions[key] = event
        elif stage in ("decision_state_committed", "decision_discarded", "decision_failed"):
            if not _integer(identifier) or identifier in terminals:
                result["errors"].append("invalid or duplicate decision terminal receipt")
            else:
                terminals[identifier] = event
    clock_times = {}
    for room, sequence in clocks.items():
        sequence.sort(key=lambda item: item["wall_us"])
        for old, new in zip(sequence, sequence[1:]):
            if new["previous_market_ms"] < old["market_time_ms"]:
                result["errors"].append(f"clock-visible intervals overlap in {room}")
        clock_times[room] = [event["market_time_ms"] for event in sequence]
        if end_wall_us is not None:
            visible = [event for event in sequence if event["wall_us"] <= end_wall_us]
            if visible:
                result["cutoff_market_by_room"][room] = visible[-1]["market_time_ms"]
    for identifier in terminals:
        if identifier not in decisions: result["errors"].append("orphan decision terminal receipt")
    seen_commands = set()
    for (identifier, index), event in actions.items():
        decision = decisions.get(identifier)
        if decision is None or index >= decision["action_count"]:
            result["errors"].append("orphan or out-of-range durable action receipt")
            continue
        key = (decision["room_id"], event["command_seq"])
        if key in seen_commands: result["errors"].append("duplicate durable command sequence")
        seen_commands.add(key)
        if event["wall_us"] < decision["wall_us"]:
            result["errors"].append("durable action precedes its decision")
    all_dispositions, bar_dispositions = Counter(), Counter()
    accepted_latency, rejected_latency, decision_latency, no_order_latency = [], [], [], []
    excluded, no_order, bar_no_order = Counter(), Counter(), Counter()
    progress = defaultdict(dict)
    anchors = 0
    for identifier, decision in decisions.items():
        room, close = decision["room_id"], decision.get("bar_close_ms")
        anchor = None
        if close is None:
            excluded["non_bar_decisions"] += 1
        else:
            index = bisect.bisect_left(clock_times.get(room, []), close)
            sequence = clocks.get(room, [])
            if index < len(sequence) and sequence[index]["previous_market_ms"] < close:
                anchor = sequence[index]["wall_us"]
            if anchor is None:
                if sequence and close <= sequence[0]["previous_market_ms"]:
                    excluded["prewarm_bar_decisions"] += 1
                else:
                    excluded["unanchored_bar_decisions"] += 1
                    result["errors"].append("new closed-bar decision has no clock publication anchor")
            elif anchor > decision["wall_us"]:
                result["errors"].append("decision precedes its closed-bar publication")
                anchor = None
            elif ((start_wall_us is not None and anchor < start_wall_us) or
                  (end_wall_us is not None and anchor > end_wall_us)):
                excluded["outside_window_bar_decisions"] += 1
                anchor = None
        if anchor is not None:
            anchors += 1
            decision_latency.append((decision["wall_us"] - anchor) / 1000)
        terminal = terminals.get(identifier)
        terminal_stage = terminal.get("stage") if terminal else None
        if terminal is not None and terminal["wall_us"] < decision["wall_us"]:
            result["errors"].append("terminal outcome precedes its decision")
        if decision["action_count"] == 0:
            no_order[terminal_stage or "pending"] += 1
            if anchor is not None:
                bar_no_order[terminal_stage or "pending"] += 1
            if anchor is not None and terminal_stage == "decision_state_committed":
                no_order_latency.append((terminal["wall_us"] - anchor) / 1000)
        elif terminal_stage == "decision_state_committed":
            result["errors"].append("nonempty decision has only a state-committed outcome")
        for index in range(decision["action_count"]):
            receipt = actions.get((identifier, index))
            if receipt:
                if terminal_stage in ("decision_discarded", "decision_failed"):
                    result["errors"].append("durable action conflicts with discarded/failed decision")
                outcome = receipt["outcome"]
            else:
                outcome = {"decision_discarded": "discarded", "decision_failed": "failed"}.get(terminal_stage, "pending")
            all_dispositions[outcome] += 1
            if anchor is not None:
                bar_dispositions[outcome] += 1
                elapsed = (receipt["wall_us"] - anchor) / 1000 if receipt else None
                coverage.record(anchor, room, decision["participant_id"], outcome, elapsed)
                if receipt:
                    (accepted_latency if outcome == "accepted" else rejected_latency).append(elapsed)
        # Progress is a committed fact, including account wakes with no new bar.
        # Drain outcomes complete latency receipts but cannot erase cutoff backlog.
        committed_wall_us = None
        if decision["action_count"] == 0 and terminal_stage == "decision_state_committed":
            committed_wall_us = terminal["wall_us"]
        elif decision["action_count"] > 0 and terminal_stage is None:
            admitted = [actions.get((identifier, index)) for index in range(decision["action_count"])]
            if all(admitted):
                committed_wall_us = max(event["wall_us"] for event in admitted)
        fields = ("evaluated_bars", "last_closed_bar_ms", "bar_interval_ms")
        if any(decision.get(field) is not None for field in fields):
            evaluated, last_close, interval = (decision.get(field) for field in fields)
            if (not _integer(evaluated) or not _integer(interval) or interval == 0
                    or (last_close is not None and not _integer(last_close))
                    or (evaluated > 0 and last_close is None)):
                result["errors"].append("invalid cumulative decision progress")
            elif (end_wall_us is not None and committed_wall_us is not None
                    and decision["wall_us"] <= committed_wall_us <= end_wall_us):
                participant = decision["participant_id"]
                old = progress[room].get(participant)
                if old is None or (committed_wall_us, identifier) > (old["committed_wall_us"], old["decision_id"]):
                    progress[room][participant] = {
                        "evaluated_bars": evaluated, "last_closed_bar_ms": last_close,
                        "bar_interval_ms": interval, "committed_wall_us": committed_wall_us,
                        "decision_id": identifier}
    result.update(decisions=len(decisions), anchored_bar_decisions=anchors,
                  whole_run_action_dispositions=dict(all_dispositions),
                  bar_cohort_action_dispositions=dict(bar_dispositions),
                  no_order_decisions=dict(no_order), bar_cohort_no_order_dispositions=dict(bar_no_order),
                  excluded=dict(excluded), at_cutoff_progress=dict(progress),
                  accepted_bar_close_to_durable=percentiles(accepted_latency),
                  rejected_bar_close_to_durable=percentiles(rejected_latency),
                  no_order_bar_close_to_state_durable=percentiles(no_order_latency),
                  bar_close_to_decision=percentiles(decision_latency),
                  accepted_window_coverage=coverage.finish())
    result["complete_generated_action_receipts"] = bool(decisions) and all_dispositions["pending"] == 0
    result["complete_decision_outcomes"] = result["complete_generated_action_receipts"] and no_order["pending"] == 0
    result["accepted_latency_exercised"] = bool(accepted_latency)
    result["qualified"] = (not result["errors"] and result["complete_decision_outcomes"]
        and result["accepted_latency_exercised"] and bar_dispositions["failed"] == 0
        and bar_dispositions["discarded"] == 0
        and bar_dispositions["pending"] == 0
        and all(bar_no_order[outcome] == 0 for outcome in ("decision_failed", "decision_discarded", "pending"))
        and result["accepted_bar_close_to_durable"]["p95_ms"] <= max_accepted_p95_ms
        and (not require_sustained_coverage or result["accepted_window_coverage"]["qualified"]))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-accepted-p95-ms", type=float, default=250)
    parser.add_argument("--start-wall-us", type=int)
    parser.add_argument("--end-wall-us", type=int)
    args = parser.parse_args()
    report = audit_receipts(args.trace, start_wall_us=args.start_wall_us, end_wall_us=args.end_wall_us,
                            max_accepted_p95_ms=args.max_accepted_p95_ms)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"qualified": report["qualified"], "errors": report["errors"]}))
    return 0 if report["qualified"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
