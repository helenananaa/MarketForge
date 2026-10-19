"""Bounded speculative sessions for explicitly qualified source hashes only.

The full frozen transcript remains durable. A restart, eviction, changed prefix
or uncommitted speculative result rebuilds from that transcript.
"""
from collections import OrderedDict
import copy
from functools import lru_cache
import hashlib
import json
from pathlib import Path

SESSIONS = OrderedDict()
QUALIFIED = {}  # mtime_ns -> qualified source hashes
# ExternalSession.append options the runtime accepts: 3 stream_tails, intents_from
# and include_output; 2 intents_from and include_output; 1 include_output; 0 none.
APPEND_LEVEL = None
IDENTITIES = {}  # (participant, seed, config, source hash) -> session key
MAX_SESSIONS = 512
COUNTERS = {"rebuilds": 0, "appends": 0, "fallbacks": 0, "evaluated_bars": 0}
LAST_EVALUATION = None  # Optional worker-envelope telemetry, never durable state.


@lru_cache(maxsize=1024)
def source_sha256(source):
    return hashlib.sha256(source.encode()).hexdigest()


def qualified(source):
    path = Path(__file__).with_name("incremental-qualified.json")
    if not path.is_file():
        return False
    stamp = path.stat().st_mtime_ns
    if stamp not in QUALIFIED:
        QUALIFIED.clear()
        QUALIFIED[stamp] = set(json.loads(path.read_text(encoding="utf-8"))["source_sha256"].values())
    return source_sha256(source) in QUALIFIED[stamp]


def append(session, bars, accounts, passes, intents_from=0):
    """Append without plot output, returning only the intents from index
    `intents_from` on and their total, where the runtime supports it; the
    broker reads only intents from a session result. `passes(tails)` builds
    the execution passes, with stream tails where the runtime takes them."""
    global APPEND_LEVEL
    for level in (3, 2, 1, 0) if APPEND_LEVEL is None else (APPEND_LEVEL,):
        options = ({"include_output": False, "intents_from": intents_from, "stream_tails": True} if level == 3 else
                   {"include_output": False, "intents_from": intents_from} if level == 2 else
                   {"include_output": False} if level == 1 else {})
        try:
            result = session.append(bars, accounts, execution_passes=passes(level == 3), **options)
        except TypeError:
            if APPEND_LEVEL is not None:
                raise
            continue
        APPEND_LEVEL = level
        if level < 2:
            result["intent_count"] = len(result["intents"])
            result["intents"] = result["intents"][intents_from:]
        return result
    raise TypeError("ExternalSession.append rejected every supported signature")


def session_key(request, source):
    marker = (json.dumps(request["participant"], sort_keys=True), request["seed"],
              json.dumps(request["config"], sort_keys=True), source_sha256(source))
    key = IDENTITIES.get(marker)
    if key is None:
        identity = json.dumps((request["participant"], request["seed"], request["config"], source),
                              sort_keys=True, separators=(",", ":"))
        if len(IDENTITIES) >= 4096:
            IDENTITIES.clear()
        key = IDENTITIES[marker] = hashlib.sha256(identity.encode()).hexdigest()
    return key


def intent_json(intent):
    # One item of the broker's digest encoding of the intent list.
    return json.dumps(intent, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def finished(hasher):
    hasher = hasher.copy()
    hasher.update(b"]")
    return hasher.hexdigest()


def replay(program, source, request, bars, accounts, *, pass_factory=None, execution_transcript=None,
           share_history=False, **kwargs):
    """Materialize the frozen passes only when full reconstruction is needed."""
    if pass_factory is not None:
        kwargs["execution_passes"] = pass_factory(0)
    return program.run_external(bars, accounts, **kwargs)


def evaluate(program, source, request, bars, accounts, *, pass_factory=None, execution_transcript=None,
             share_history=False, **kwargs):
    """A session result holds only the intents after those of earlier appends,
    with `prefix_digest` (of the intents before the request's committed bars)
    and `intents_digest` (of all of them) in the broker's digest encoding.

    With `share_history`, the caller guarantees that history items are never
    changed in place, so the session keeps them instead of copies and its
    prefix checks compare by identity."""
    global LAST_EVALUATION
    reason = ("runtime_no_session" if not hasattr(program, "external_session") else
              "source_unqualified" if not qualified(source) else None)
    if reason is not None:
        LAST_EVALUATION = {"mode": "replay", "reason": reason}
        COUNTERS["fallbacks"] += 1
        return replay(program, source, request, bars, accounts, pass_factory=pass_factory,
                      execution_transcript=execution_transcript, **kwargs)
    keep = (lambda value: value) if share_history else copy.deepcopy
    passes = kwargs.get("execution_passes")
    key = session_key(request, source)
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
        entry = {"session": program.external_session(**settings), "intent_count": 0, "digest": hashlib.sha256(b"[")}
        COUNTERS["rebuilds"] += 1
    # Do not put a failed session back in the cache. No orders execute here.
    if pass_factory is not None:
        new_passes = lambda tails: pass_factory(offset, tails=tails)
    else:
        new_passes = lambda tails: None if passes is None else passes[offset:]
    count = entry["intent_count"]
    result = append(entry["session"], bars[offset:], accounts[offset:], new_passes, count)
    COUNTERS["evaluated_bars"] += len(bars) - offset
    fresh, hasher = result["intents"], entry["digest"]
    if result["intent_count"] != count + len(fresh):
        raise ValueError("incremental session intent count mismatch")
    # Intents come in bar order. Earlier appends evaluated only committed bars,
    # whose intents a new bar cannot change.
    if matching and any(intent["bar_index"] < previous for intent in fresh):
        raise ValueError("previous Pine intents changed during reconstruction")
    split = next((n for n, intent in enumerate(fresh) if intent["bar_index"] >= previous), len(fresh))
    if any(intent["bar_index"] < previous for intent in fresh[split:]):
        # Out of bar order: digest the committed intents as the broker filters them.
        prefix = [intent for intent in fresh if intent["bar_index"] < previous]
        prefix_digest = hashlib.sha256(json.dumps(prefix, sort_keys=True, separators=(",", ":"),
                                                  allow_nan=False).encode()).hexdigest()
        split = None
    for n, intent in enumerate(fresh):
        if n == split:
            prefix_digest = finished(hasher)
        hasher.update((b"," if count + n else b"") + intent_json(intent))
    if split == len(fresh):
        prefix_digest = finished(hasher)
    result.update(prefix_digest=prefix_digest, intents_digest=finished(hasher))
    entry["intent_count"] = result["intent_count"]
    if matching:
        # Prefix equality was checked above. Unless history is shared, retain
        # copies of new frames only; consumer input cannot alias the cache.
        entry["bars"].extend(keep(bars[previous:]))
        entry["accounts"].extend(keep(accounts[previous:]))
    else:
        entry.update(bars=list(keep(bars)), accounts=list(keep(accounts)), settings=copy.deepcopy(settings))
    if pass_factory is not None:
        # Keep the compact transcript, not O(history squared) request-data prefixes.
        if matching:
            entry["evaluations"].extend(keep(evaluations[previous:]))
            for provider_key, values in providers.items():
                target = entry["providers"].setdefault(provider_key, [])
                target.extend(keep(values[len(target):]))
        else:
            entry.update(evaluations=list(keep(evaluations)),
                         providers={k: list(keep(v)) for k, v in providers.items()})
    else:
        if matching:
            entry["passes"].extend(keep(passes[previous:]))
        else:
            entry["passes"] = list(keep(passes))
    SESSIONS[key] = entry
    while len(SESSIONS) > MAX_SESSIONS:
        SESSIONS.popitem(last=False)
    return result
