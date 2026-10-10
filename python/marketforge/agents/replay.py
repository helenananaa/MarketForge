"""Replay recorded business intents against an isolated real exchange, without a model."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from unittest.mock import patch

from marketforge import Client
from .runtime import TradingService
from .storage import encode


VERSION = "marketforge.agent-replay.v1"
REPLAY_TOOLS = {"context", "decision_begin", "receipt", "market_read", "trade", "alert_set", "alert_cancel", "alerts", "note", "announce", "wait", "orders", "fills", "order_cancel_all", "policy_status"}


@contextmanager
def isolated_exchange(executable):
    """Never addresses an existing server or inherits its journal/credentials."""
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0)); port = sock.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="marketforge-replay-") as folder:
        env = {k: v for k, v in os.environ.items() if not k.startswith("MARKETFORGE_")}
        env["MARKETFORGE_BIND_ADDR"] = f"127.0.0.1:{port}"
        with open(Path(folder) / "exchange.log", "w", encoding="utf-8") as log:
            process = subprocess.Popen([str(Path(executable).resolve())], env=env, cwd=folder,
                stdout=log, stderr=subprocess.STDOUT, creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0)
            client = Client(f"http://127.0.0.1:{port}", timeout=5)
            try:
                deadline = time.monotonic() + 15
                while True:
                    try:
                        client.health_ready(); break
                    except OSError:
                        if process.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError("isolated exchange did not become ready") from None
                        time.sleep(0.1)
                yield client
            finally:
                if process.poll() is None:
                    process.terminate()
                process.wait(5)


def domain_result(name, result):
    if name in {"context", "decision_begin"}:
        return {"generation": result["generation"], "observations": result["observations"]}
    if name == "alert_set":
        return {k: v for k, v in result.items() if k not in {"version", "last_trigger_at"}}
    if name == "alerts":
        return [{k: v for k, v in alert.items() if k not in {"version", "last_trigger_at"}} for alert in result]
    return result


class Recorder:
    def __init__(self, scenario, trader):
        self.trace = {"version": VERSION, "scenario": scenario, "trader": trader, "steps": []}
        self.leases = {}

    def tool(self, name, args, result=None, error=None):
        if name not in REPLAY_TOOLS:
            raise ValueError("unsupported replay tool; record market/business tools only")
        arguments = dict(args)
        if "decision_id" in arguments:
            arguments["decision_id"] = self.leases[arguments["decision_id"]]
        step = {"kind": "tool", "name": name, "arguments": arguments, "wall_time": time.time()}
        if error:
            step["error"] = type(error).__name__
        else:
            step["expected"] = domain_result(name, result)
            if name == "decision_begin":
                alias = "decision-" + str(len(self.leases))
                self.leases[result["decision_id"]] = alias
                step["bind"] = alias
        self.trace["steps"].append(step)

    def market(self, instrument, command):
        self.trace["steps"].append({"kind": "market", "instrument": instrument, "command": command, "wall_time": time.time()})

    def clock(self, steps):
        self.trace["steps"].append({"kind": "clock", "steps": steps, "wall_time": time.time()})

    def poll(self, generation):
        self.trace["steps"].append({"kind": "poll", "generation": generation, "wall_time": time.time()})

    def save(self, path):
        Path(path).write_text(json.dumps(self.trace, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n")


def replay(trace, executable):
    if trace.get("version") != VERSION or not isinstance(trace.get("steps"), list):
        raise ValueError("unsupported replay recording")
    if not 1 <= len(trace["steps"]) <= 10000:
        raise ValueError("replay must contain 1-10000 ordered steps")
    trader_config = dict(trace["trader"], backend="external", exchange_token_env="")
    trader, room = trader_config["id"], trace["scenario"]["room_id"]
    if trader_config["room"] != room:
        raise ValueError("replay trader and scenario room do not match")
    results, leases, previous_time = [], {}, 0
    with isolated_exchange(executable) as admin, tempfile.TemporaryDirectory() as directory:
        admin._request("POST", "/rooms", {"scenario": trace["scenario"], "autostart_agents": False})
        admin.add_member(room, "agent-" + trader, "trader")
        admin.assign_account(room, trader_config["account_id"], "agent-" + trader)
        service = TradingService(directory, None, admin.base_url)
        service.create(trader_config)
        # Replay has a single ordered driver: no background clock, monitors or strategies.
        config = service.config(trader); config["status"] = "running"; service.store.put("trader", trader, config)
        try:
            for index, step in enumerate(trace["steps"]):
                wall_time = step["wall_time"]
                if type(wall_time) not in (int, float) or wall_time < previous_time:
                    raise ValueError("recording time went backwards")
                previous_time = wall_time
                with patch("time.time", return_value=wall_time):
                    kind = step["kind"]
                    if kind == "market":
                        result = admin._request("POST", f"/rooms/{room}/instruments/{step['instrument']}/orders",
                            step["command"], idempotency_key=f"replay-market-{index}")
                    elif kind == "clock":
                        result = admin.advance_clock(room, step["steps"], idempotency_key=f"replay-clock-{index}")
                    elif kind == "poll":
                        service.alerts.poll(trader)
                        result = {"generation": service.alerts.state(trader)["generation"]}
                        if result["generation"] != step["generation"]:
                            raise AssertionError(f"alert generation mismatch at step {index}")
                    elif kind == "tool":
                        name, args = step["name"], dict(step["arguments"])
                        if name not in REPLAY_TOOLS:
                            raise ValueError("replay cannot execute framework, web or strategy code")
                        if "decision_id" in args:
                            if args["decision_id"] not in leases:
                                raise ValueError("decision used before it was observed")
                            args["decision_id"] = leases[args["decision_id"]]
                        try:
                            value = service.external_call(trader, name, args)
                        except (ValueError, KeyError, TypeError) as exc:
                            if step.get("error") != type(exc).__name__:
                                raise
                            result = {"error": type(exc).__name__}
                        else:
                            if step.get("error"):
                                raise AssertionError(f"expected rejection was accepted at step {index}")
                            if name == "decision_begin":
                                leases[step["bind"]] = value["decision_id"]
                            result = domain_result(name, value)
                            if "expected" in step and result != step["expected"]:
                                raise AssertionError(f"business result mismatch at step {index} ({name})")
                    else:
                        raise ValueError("unknown replay step")
                    results.append({"step": index, "kind": kind, "result": result})
            final = service.observations(config)
            output = {"steps": len(results), "results": results, "final_observations": final}
            return output | {"digest": hashlib.sha256(encode(output).encode()).hexdigest(), "provider_inference": False}
        finally:
            service.store.db.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recording")
    parser.add_argument("--exchange-server", default="target/debug/exchange-server.exe" if os.name == "nt" else "target/debug/exchange-server")
    parser.add_argument("--runs", type=int, default=2)
    parser.add_argument("--report", required=True)
    args = parser.parse_args()
    if not 1 <= args.runs <= 20:
        parser.error("runs must be 1-20")
    trace = json.loads(Path(args.recording).read_text(encoding="utf-8"))
    runs = [replay(trace, args.exchange_server) for _ in range(args.runs)]
    report = {"version": VERSION, "recording_sha256": hashlib.sha256(encode(trace).encode()).hexdigest(),
        "matched": len({run["digest"] for run in runs}) == 1, "runs": runs}
    Path(args.report).write_text(json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8", newline="\n")
    if not report["matched"]:
        raise SystemExit("replay digests differ")
    print(json.dumps({"matched": report["matched"], "runs": len(runs), "digest": runs[0]["digest"]}))


if __name__ == "__main__":
    main()
