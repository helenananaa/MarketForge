"""Exact, bounded reconstruction of host input deltas. Never caches bot output.

Patches are applied copy-on-write: every container on a patch path is replaced
by a fresh copy, and no decoded object is ever changed in place. An object that
is still the same object as in an earlier request therefore still holds the
same content, which lets consumers compare unchanged history by identity.
"""
from collections import OrderedDict
import copy
import json
import math
from operator import is_

MAX_REQUESTS = 512
MAX_PATCHES = 256


def _own(container, fresh):
    """A copy of `container` that this decode may change, made once per decode."""
    if id(container) in fresh:
        return container
    if type(container) is dict:
        owned = dict(container)
    elif type(container) is list:
        owned = list(container)
    else:
        raise ValueError("invalid worker input path")
    fresh[id(owned)] = owned  # keeps the copy alive, so its id stays unique
    return owned


def apply_patches(root, patches, prefix=()):
    """Apply set/remove/append patches below `prefix` without changing any input
    object; returns the new root."""
    fresh = {}
    root = _own(root, fresh)
    for patch in patches:
        if not isinstance(patch["path"], list) or not patch["path"] or len(patch["path"]) + len(prefix) > 128:
            raise ValueError("invalid worker input path")
        path = [*prefix, *patch["path"]]
        parent = root
        for part in path[:-1]:
            child = _own(parent[part], fresh)
            parent[part] = child
            parent = child
        last = path[-1]
        if patch["op"] == "set":
            parent[last] = patch["value"]
        elif patch["op"] == "remove":
            del parent[last]
        elif patch["op"] == "append":
            target = parent[last]
            if not isinstance(target, list) or len(target) != patch["from"] or not isinstance(patch["values"], list):
                raise ValueError("worker input append boundary")
            target = _own(target, fresh)
            target.extend(patch["values"])
            parent[last] = target
        else:
            raise ValueError("invalid worker input operation")
    return root


class RequestDecoder:
    def __init__(self):
        self.requests = OrderedDict()

    def decode(self, envelope):
        return self._decode(envelope, True)

    def decode_readonly(self, envelope):
        """Borrow host input only for the synchronous Pine worker consumer.

        The consumer must leave every input field unchanged and serialize its
        response before the next decode. Generic consumers keep decode(),
        which supplies independent mutable input.
        """
        return self._decode(envelope, False)

    def _decode(self, envelope, copy_input):
        key = envelope.get("cache_key")
        if "request" in envelope:
            if "request_delta" in envelope or "base" in envelope:
                raise ValueError("ambiguous worker request")
            request = envelope["request"]
        else:
            patches = envelope.get("request_delta")
            entry = self.requests.pop(key, None)
            if entry is None or entry[0] != envelope.get("base"):
                raise ValueError("worker input delta base mismatch")
            if not isinstance(patches, list) or len(patches) > MAX_PATCHES:
                raise ValueError("worker input patch limit")
            request = apply_patches(entry[1], patches)
        if key is not None:
            if not isinstance(key, str) or len(key) != 64:
                raise ValueError("invalid worker input cache key")
            self.requests.pop(key, None)
            self.requests[key] = (envelope["id"], request)
            while len(self.requests) > MAX_REQUESTS:
                self.requests.popitem(last=False)
            # Consumers receive independent input. Their mutations and responses
            # cannot become the base for a later host-supplied delta.
            return copy.deepcopy(request) if copy_input else request
        return request

    def fold_state(self, key, patches):
        """Make the cached request's state the decision's new state, as the host
        does once it accepts the reply carrying these state patches. The next
        delta is then relative to that state."""
        identity, request = self.requests[key]
        self.requests[key] = (identity, apply_patches(request, patches, ("state",)))


class StateLengths:
    """json.dumps lengths of states folded into decoder bases. A request state
    built from such a base by patches shares every unchanged value with it, so
    only the changed top-level values are measured."""

    def __init__(self):
        self.states = OrderedDict()

    def length(self, key, state):
        cached = self.states.get(key)
        if cached is None or type(state) is not dict:
            return _length(state)
        base, length = cached
        length += 2 * (max(0, len(state) - 1) - max(0, len(base) - 1))
        for name, value in state.items():
            if name not in base:
                length += _length(name) + 2 + _length(value)
            elif value is not base[name]:
                length += _length(value) - _length(base[name])
        for name, value in base.items():
            if name not in state:
                length -= _length(name) + 2 + _length(value)
        return length

    def remember(self, key, state, length):
        self.states.pop(key, None)
        self.states[key] = (state, length)
        while len(self.states) > MAX_REQUESTS:
            self.states.popitem(last=False)


class NotDelta(Exception):
    """The new state cannot be described exactly as JSON patches."""


def canonical(value):
    """Whether `value` is JSON in the form a JSON parser returns: the host and
    this process then hold exactly equal values after a round trip."""
    kind = type(value)
    if kind is dict:
        return all(type(key) is str and canonical(item) for key, item in value.items())
    if kind is list:
        return all(map(canonical, value))
    if kind is float:
        return math.isfinite(value)
    return kind in (str, int, bool) or value is None


def same(old, new):
    """JSON equality that, like the host, distinguishes 1 from 1.0 and -0.0 from 0.0."""
    if old is new:
        return True
    kind = type(old)
    if kind is not type(new):
        return False
    if kind is dict:
        return len(old) == len(new) and all(key in new and same(item, new[key]) for key, item in old.items())
    if kind is list:
        return len(old) == len(new) and all(map(same, old, new))
    if kind is float:
        return old == new and math.copysign(1.0, old) == math.copysign(1.0, new)
    return old == new


def _length(value):
    # json.dumps defaults, as the full Pine response is encoded.
    return len(json.dumps(value, allow_nan=False))


def state_delta(old, new):
    """Patches from `old` to `new` and the change in json.dumps length.

    Unchanged objects are recognized by identity first, so history that the
    decision only appended to costs its new items, not its length."""
    if type(old) is not dict or type(new) is not dict:
        raise NotDelta()
    patches = []
    change = _diff(old, new, [], patches)
    return patches, change


def _diff(old, new, path, patches):
    if old is new:
        return 0
    if type(old) is dict and type(new) is dict:
        change = 2 * (max(0, len(new) - 1) - max(0, len(old) - 1))  # ", " separators
        for key in old:
            if key not in new:
                if type(key) is not str:
                    raise NotDelta()
                change -= _length(key) + 2 + _length(old[key])
                patches.append({"op": "remove", "path": [*path, key]})
        for key, value in new.items():
            if type(key) is not str:
                raise NotDelta()
            if key in old:
                change += _diff(old[key], value, [*path, key], patches)
            else:
                if not canonical(value):
                    raise NotDelta()
                change += _length(key) + 2 + _length(value)
                patches.append({"op": "set", "path": [*path, key], "value": value})
        return change
    if type(old) is list and type(new) is list and len(new) >= len(old) and (
            all(map(is_, old, new)) or all(map(same, old, new))):
        if len(new) == len(old):
            return 0
        tail = new[len(old):]
        if not canonical(tail):
            raise NotDelta()
        patches.append({"op": "append", "path": path, "from": len(old), "values": tail})
        # "[a]" -> "[a, b, c]": each new item and its ", " separator.
        return _length(tail) - 2 + (2 if old else 0)
    if same(old, new):
        return 0
    if not canonical(new):
        raise NotDelta()
    patches.append({"op": "set", "path": path, "value": new})
    return _length(new) - _length(old)

