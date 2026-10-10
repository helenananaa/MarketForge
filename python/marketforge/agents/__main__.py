"""Local operator API. python -m marketforge.agents --help"""
import argparse
import hmac
import json
import os
import secrets
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from .runtime import Runtime, TradingService, identifier


def handler(runtime, token, origins):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass  # Never log Authorization, connection keys or request bodies.

        def reply(self, status, body):
            data = json.dumps(body, ensure_ascii=False, allow_nan=False).encode()
            self.send_response(status)
            origin = self.headers.get("Origin")
            if origin in origins:
                self.send_header("Access-Control-Allow-Origin", origin)
                self.send_header("Vary", "Origin")
            self.send_header("Access-Control-Allow-Headers", "Authorization,Content-Type")
            self.send_header("Access-Control-Allow-Methods", "GET,POST,OPTIONS")
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_OPTIONS(self):
            self.reply(200 if self.headers.get("Origin") in origins else 403, {})

        def do_GET(self):
            self.dispatch(False)

        def do_POST(self):
            self.dispatch(True)

        def dispatch(self, write):
            origin = self.headers.get("Origin")
            if origin and origin not in origins:
                return self.reply(403, {"error": "origin is not allowed"})
            try:
                url = urlsplit(self.path)
                parts = url.path.strip("/").split("/")
                authorization = self.headers.get("Authorization", "")
                operator = hmac.compare_digest(authorization, "Bearer " + token)
                scoped = len(parts) == 3 and parts[0] == "tools" and authorization.startswith("Bearer ") and runtime.authorized(parts[1], authorization[7:])
                if not operator and not scoped:
                    return self.reply(401, {"error": "appropriate service or trader tool token required"})
                args = {}
                if write:
                    length = int(self.headers.get("Content-Length", "0"))
                    if not 0 < length <= 262144:
                        raise ValueError("request body must be 1-262144 bytes")
                    args = json.loads(self.rfile.read(length))
                    if not isinstance(args, dict):
                        raise ValueError("expected JSON object")
                if len(parts) == 3 and parts[0] == "tools":
                    trader = identifier(parts[1])
                    runtime.config(trader)
                    if parts[2] == "schema" and not write:
                        result = runtime.tool_schemas()
                    elif parts[2] == "connection" and write:
                        result = runtime.connection_update(trader, args)
                    elif parts[2] == "runtime" and not write:
                        result = runtime.runtime_status(trader)
                    elif parts[2] == "model" and write:
                        result = runtime.model_control(trader, args)
                    elif parts[2] == "call" and write:
                        if set(args) != {"name", "arguments"} or not isinstance(args["arguments"], dict):
                            raise ValueError("expected name and arguments")
                        result = runtime.external_call(trader, args["name"], args["arguments"], connection_id=self.headers.get("X-MarketForge-Connection"))
                    elif parts[2] == "events" and not write:
                        query = parse_qs(url.query)
                        after = max(0, int(query.get("after", [0])[0]))
                        # Durable cursor API is also an event subscription via bounded long polling.
                        wait = min(20, max(0, float(query.get("wait", [0])[0])))
                        import time
                        deadline = time.monotonic() + wait
                        while True:
                            result = runtime.store.events(trader, after, tail=query.get("tail") == ["1"])
                            if result or time.monotonic() >= deadline:
                                break
                            time.sleep(0.1)
                    else:
                        return self.reply(404, {"error": "unknown tool endpoint"})
                elif parts == ["status"] and not write:
                    result = {"protocol_version": "agent.v1", "exchange_url": runtime.exchange_url,
                              "sandbox": runtime.sandbox.check(), "plugins": [p[0] for p in runtime.plugins.values()],
                              "connections": [{"id": k, "model": v["model"], "plugin_id": v["plugin_id"]} for k, v in runtime.connections.items()]}
                elif parts == ["connections", "test"] and write:
                    result = runtime.connect(args)
                elif parts == ["traders"]:
                    result = runtime.create(args) if write else runtime.store.all("trader")
                elif len(parts) == 4 and parts[0] == "traders" and parts[2] == "projects" and not write:
                    trader, name = identifier(parts[1]), identifier(parts[3])
                    config = runtime.config(trader)
                    project = runtime.execute(config, "operator-read", "strategy_read", {"name": name}, "direct")
                    state = runtime.execute(config, "operator-read", "strategy_status", {"name": name}, "direct")
                    result = {"project": project, "state": state}
                elif len(parts) == 3 and parts[0] == "traders":
                    trader = identifier(parts[1])
                    runtime.config(trader)
                    if parts[2] == "start" and write:
                        result = runtime.start(trader)
                    elif parts[2] == "stop" and write:
                        result = runtime.stop(trader)
                    elif parts[2] == "events" and not write:
                        query = parse_qs(url.query)
                        after = max(0, int(query.get("after", [0])[0]))
                        until = min(2**63-1, int(query.get("until", [2**63-1])[0]))
                        result = runtime.store.events(trader, after, tail=query.get("tail") == ["1"], until=until)
                    elif parts[2] == "strategies" and not write:
                        result = runtime.strategies(trader)
                    elif parts[2] == "alerts" and not write:
                        result = runtime.alerts.list(trader)
                    elif parts[2] == "runtime" and not write:
                        result = runtime.runtime_status(trader)
                    elif parts[2] == "policy":
                        result = runtime.policy_update(trader, args) if write else runtime.policy_status(trader)
                    elif parts[2] == "access" and write:
                        result = runtime.issue_access(trader)
                    elif parts[2] == "backend" and write:
                        if args != {"backend": "external"}:
                            raise ValueError("only explicit migration to external backend is supported")
                        result = runtime.migrate_external(trader)
                    else:
                        return self.reply(404, {"error": "unknown endpoint"})
                else:
                    return self.reply(404, {"error": "unknown endpoint"})
                self.reply(200, result)
            except (ValueError, KeyError, TypeError) as exc:
                self.reply(400, {"error": str(exc)[:1000]})
            except Exception as exc:
                # Provider exceptions may contain echoed secrets. Report only type.
                self.reply(502, {"error": f"{type(exc).__name__}: connection or execution failed"})
    return Handler


def main():
    parser = argparse.ArgumentParser(description="MarketForge agent.v1 local trader service")
    parser.add_argument("--port", type=int, default=57306)
    parser.add_argument("--exchange-url", default="http://127.0.0.1:57305")
    parser.add_argument("--data-dir", default=".local/agents")
    parser.add_argument("--plugin-dir", default="agent-plugins")
    parser.add_argument("--enable-legacy-model-loop", action="store_true", help="opt in to the old model harness instead of the model-free service")
    parser.add_argument("--origin", action="append", default=["http://127.0.0.1:57304", "http://localhost:57304"])
    args = parser.parse_args()
    directory = Path(args.data_dir)
    directory.mkdir(parents=True, exist_ok=True)
    # An OS advisory lock prevents duplicate services from spending one account concurrently.
    lock_file = open(directory / "service.lock", "a+b")
    lock_file.write(b"0")
    lock_file.flush()
    lock_file.seek(0)
    if os.name == "nt":
        import msvcrt
        msvcrt.locking(lock_file.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        import fcntl
        fcntl.flock(lock_file, fcntl.LOCK_EX | fcntl.LOCK_NB)
    token_path = directory / "operator.token"
    token = os.environ.get("MARKETFORGE_AGENT_OPERATOR_TOKEN") or (token_path.read_text().strip() if token_path.exists() else secrets.token_urlsafe(32))
    if len(token) < 24:
        raise ValueError("operator token must have at least 24 characters")
    token_path.write_text(token)
    token_path.chmod(0o600)
    runtime = Runtime(directory, args.plugin_dir, args.exchange_url) if args.enable_legacy_model_loop else TradingService(directory, None, args.exchange_url)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler(runtime, token, set(args.origin)))
    server.daemon_threads = True
    print(f"Agent service: http://127.0.0.1:{args.port}; operator token: {token_path.resolve()}", flush=True)
    try:
        server.serve_forever()
    finally:
        for trader in runtime.store.all("trader"):
            runtime.stop(trader["id"])
        runtime.close_workspaces()
        for _, worker in runtime.installers.values():
            worker.join(timeout=30)
        server.server_close()
        lock_file.close()


if __name__ == "__main__":
    main()
