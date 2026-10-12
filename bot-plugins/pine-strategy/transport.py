"""Exact, bounded reconstruction of host input deltas. Never caches bot output."""
from collections import OrderedDict
import copy

MAX_REQUESTS = 512
MAX_PATCHES = 256


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
            request = entry[1]
            for patch in patches:
                path = patch["path"]
                if not isinstance(path, list) or not path or len(path) > 128:
                    raise ValueError("invalid worker input path")
                parent = request
                for part in path[:-1]:
                    parent = parent[part]
                last = path[-1]
                if patch["op"] == "set":
                    parent[last] = patch["value"]
                elif patch["op"] == "remove":
                    del parent[last]
                elif patch["op"] == "append":
                    target = parent[last]
                    if not isinstance(target, list) or len(target) != patch["from"] or not isinstance(patch["values"], list):
                        raise ValueError("worker input append boundary")
                    target.extend(patch["values"])
                else:
                    raise ValueError("invalid worker input operation")
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
