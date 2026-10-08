"""Trusted, operator-installed adapter; model-created strategies never load here."""
import json
import urllib.request
from urllib.parse import urlsplit


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError("model endpoint redirects are disabled")


def complete(connection, messages, tools):
    base = connection["base_url"].rstrip("/")
    url = urlsplit(base)
    if url.scheme not in ("http", "https") or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise ValueError("invalid model endpoint")
    if url.scheme == "http" and url.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise ValueError("remote model endpoints require HTTPS")
    body = {"model": connection["model"], "messages": messages, "max_tokens": 2048}
    if tools:
        body.update(tools=tools, tool_choice="auto")
    headers = {"Content-Type": "application/json"}
    if connection.get("api_key"):
        headers["Authorization"] = "Bearer " + connection["api_key"]
    request = urllib.request.Request(base + "/chat/completions", data=json.dumps(body).encode(), headers=headers)
    with urllib.request.build_opener(NoRedirect()).open(request, timeout=90) as response:
        raw = response.read(1_048_577)
    if len(raw) > 1_048_576:
        raise ValueError("model response exceeds 1 MiB")
    result = json.loads(raw)
    message = result["choices"][0]["message"]
    if not isinstance(message, dict) or len(message.get("tool_calls", [])) > 16:
        raise ValueError("invalid model tool response")
    return {"role": "assistant", "content": message.get("content"),
            **({"tool_calls": message["tool_calls"]} if message.get("tool_calls") else {})}, result.get("usage", {})
