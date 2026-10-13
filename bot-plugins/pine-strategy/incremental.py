"""Bounded speculative sessions for explicitly qualified source hashes only.

The full frozen transcript remains durable. A restart, eviction, changed prefix
or uncommitted speculative result rebuilds from that transcript.
"""
from collections import OrderedDict
import copy
import hashlib
import json
from pathlib import Path

SESSIONS = OrderedDict()
MAX_SESSIONS = 512
COUNTERS = {"rebuilds": 0, "appends": 0, "fallbacks": 0, "evaluated_bars": 0}
LAST_EVALUATION = None  # Optional worker-envelope telemetry, never durable state.


def qualified(source):
    path = Path(__file__).with_name("incremental-qualified.json")
    if not path.is_file():
        return False
    digest = hashlib.sha256(source.encode()).hexdigest()
    return digest in json.loads(path.read_text(encoding="utf-8"))["source_sha256"].values()


def replay(program, source, request, bars, accounts, *, pass_factory=None, execution_transcript=None, **kwargs):
    """Materialize the frozen passes only when full reconstruction is needed."""
    if pass_factory is not None:
        kwargs["execution_passes"] = pass_factory(0)
    return program.run_external(bars, accounts, **kwargs)


def evaluate(program, source, request, bars, accounts, *, pass_factory=None, execution_transcript=None, **kwargs):
    global LAST_EVALUATION
    reason = ("runtime_no_session" if not hasattr(program, "external_session") else
              "source_unqualified" if not qualified(source) else None)
    if reason is not None:
        LAST_EVALUATION = {"mode": "replay", "reason": reason}
        COUNTERS["fallbacks"] += 1
        return replay(program, source, request, bars, accounts, pass_factory=pass_factory,
                      execution_transcript=execution_transcript, **kwargs)
    passes = kwargs.get("execution_passes")
    identity = json.dumps((request["participant"], request["seed"], request["config"], source),
                          sort_keys=True, separators=(",", ":"))
    key = hashlib.sha256(identity.encode()).hexdigest()
    entry = SESSIONS.pop(key, None)
    previous = len((request.get("state") or {}).get("bars", []))
    settings = {k: v for k, v in kwargs.items() if k != "execution_passes"}
    if pass_factory is not None:
        evaluations = execution_transcript["evaluations"]
        providers = execution_transcript["providers"]
        matching_passes = (entry is not None and entry.get("evaluations") == evaluations[:previous]
            and all(key in providers and old == providers[key][:len(old)]
                    for key, old in entry.get("providers", {}).items()))
    else:
        matching_passes = entry is not None and entry.get("passes") == passes[:previous]
    matching = (entry is not None and len(entry["bars"]) == previous
                and entry["bars"] == bars[:previous]
                and entry["accounts"] == accounts[:previous]
                and matching_passes
                and entry["settings"] == settings)
    if matching:
        offset = previous
        LAST_EVALUATION = {"mode": "append", "reason": None}
        COUNTERS["appends"] += 1
    else:
        offset = 0
        LAST_EVALUATION = {"mode": "rebuild", "reason": "session_missing" if entry is None else "state_mismatch"}
        entry = {"session": program.external_session(**settings)}
        COUNTERS["rebuilds"] += 1
    # Do not put a failed session back in the cache. No orders execute here.
    new_passes = pass_factory(offset) if pass_factory is not None else passes[offset:]
    result = entry["session"].append(bars[offset:], accounts[offset:], execution_passes=new_passes)
    COUNTERS["evaluated_bars"] += len(bars) - offset
    if matching:
        # Prefix equality was checked above. Retain independently copied history
        # and copy only new frames; consumer input still cannot alias the cache.
        entry["bars"].extend(copy.deepcopy(bars[previous:]))
        entry["accounts"].extend(copy.deepcopy(accounts[previous:]))
    else:
        entry.update(bars=copy.deepcopy(bars), accounts=copy.deepcopy(accounts), settings=copy.deepcopy(settings))
    if pass_factory is not None:
        # Keep the compact transcript, not O(history squared) request-data prefixes.
        if matching:
            entry["evaluations"].extend(copy.deepcopy(evaluations[previous:]))
            for provider_key, values in providers.items():
                target = entry["providers"].setdefault(provider_key, [])
                target.extend(copy.deepcopy(values[len(target):]))
        else:
            entry.update(evaluations=copy.deepcopy(evaluations), providers=copy.deepcopy(providers))
    else:
        if matching:
            entry["passes"].extend(copy.deepcopy(passes[previous:]))
        else:
            entry["passes"] = copy.deepcopy(passes)
    SESSIONS[key] = entry
    while len(SESSIONS) > MAX_SESSIONS:
        SESSIONS.popitem(last=False)
    return result
