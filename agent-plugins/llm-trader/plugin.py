"""Trusted, operator-installed adapter; model-created strategies never load here."""
import json
import http.client
import socket
import threading
import urllib.request
from urllib.parse import urlsplit


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError("model endpoint redirects are disabled")


def complete(connection, messages, tools, cancel_event=None):
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
    finished = threading.Event()
    sockets = []
    def factory(connection_type):
        def create(*args, **kwargs):
            transport = connection_type(*args, **kwargs)
            connect = transport.connect
            def open_connection():
                connect()
                sockets.append(transport.sock)
                if cancel_event is not None and cancel_event.is_set():
                    transport.close()
                    raise ValueError("model request interrupted")
            transport.connect = open_connection
            return transport
        return create
    class HTTP(urllib.request.HTTPHandler):
        def http_open(self, request):
            return self.do_open(factory(http.client.HTTPConnection), request)
    class HTTPS(urllib.request.HTTPSHandler):
        def https_open(self, request):
            return self.do_open(factory(http.client.HTTPSConnection), request, context=self._context)
    def cancel_transport():
        while not finished.wait(0.05):
            if cancel_event.is_set():
                for sock in list(sockets):
                    try:
                        sock.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
                    sock.close()
                return
    if cancel_event is not None:
        threading.Thread(target=cancel_transport, daemon=True).start()
    try:
        if cancel_event is not None and cancel_event.is_set():
            raise ValueError("model request interrupted")
        request = urllib.request.Request(base + "/chat/completions", data=json.dumps(body).encode(), headers=headers)
        # Keep urllib's proxy/TLS handling, including the operator's proxy config.
        with urllib.request.build_opener(NoRedirect(), HTTP(), HTTPS()).open(request, timeout=90) as response:
            raw = response.read(1_048_577)
    finally:
        finished.set()
    if len(raw) > 1_048_576:
        raise ValueError("model response exceeds 1 MiB")
    result = json.loads(raw)
    message = result["choices"][0]["message"]
    if not isinstance(message, dict) or len(message.get("tool_calls", [])) > 16:
        raise ValueError("invalid model tool response")
    return {"role": "assistant", "content": message.get("content"),
            **({"tool_calls": message["tool_calls"]} if message.get("tool_calls") else {})}, result.get("usage", {})
