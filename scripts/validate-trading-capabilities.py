"""Provider-free live acceptance against a disposable local MarketForge server.

Creates a named QA room; never points this at an existing trading room.
Run with PYTHONPATH=python and the marketforge[agents] environment.
"""
import argparse
import asyncio
import base64
import json
import os
from pathlib import Path
import secrets
import sys
import threading
import time
from http.server import ThreadingHTTPServer

from marketforge import Client, MarketForgeError
from marketforge.agents.runtime import TradingService
from marketforge.agents.__main__ import handler
from marketforge.agents.mcp_server import ServiceClient


async def probe_mcp(service_url, token_path, output):
    from mcp import ClientSession, StdioServerParameters
    from mcp.client.stdio import stdio_client
    params = StdioServerParameters(command=sys.executable, args=["-m", "marketforge.agents.mcp_server",
        "--service-url", service_url, "--trader", "riskqa", "--token-file", str(token_path)], env={**os.environ, "PYTHONPATH": "python"})
    async with stdio_client(params) as (read, write):
        async with ClientSession(read, write) as session:
            await session.initialize()
            names = [t.name for t in (await session.list_tools()).tools]
            assert {"market_history", "market_indicators", "chart_export", "risk_events"} <= set(names)
            result = await session.call_tool("chart_export", {"instrument": "V-BTC-PERP", "period": 3, "width": 800, "height": 400})
            assert not result.isError, result.content
            assert result.content[0].type == "image"
            output.write_bytes(base64.b64decode(result.content[0].data, validate=True))
            assert "image_base64" not in result.structuredContent
            return len(names)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--room", default="capability-qa")
    parser.add_argument("--output", default="output/trading-capabilities")
    parser.add_argument("--strategy", action="store_true", help="also run generated Python in the existing Docker sandbox")
    args = parser.parse_args()
    if not args.base_url.startswith("http://127.0.0.1:") or not args.room.startswith("capability-qa"):
        raise ValueError("acceptance requires a disposable loopback server and capability-qa room")
    output = Path(args.output) / args.room; output.mkdir(parents=True, exist_ok=True)
    owner = Client(args.base_url, user_id="local-user")
    instrument = "V-BTC-PERP"
    scenario = {"room_id": args.room, "market": {"Perp": {
        "instrument": {"symbol": instrument, "base_asset": "V", "quote_asset": "BTC", "tick_size": 1, "lot_size": 1},
        "clearing": {"leverage": 10, "maker_fee_ppm": 0, "taker_fee_ppm": 0, "maintenance_margin_ppm": 50000, "liquidation_fee_ppm": 10000, "initial_insurance_fund": 0}, "risk": {}, "initial_mark_price_tick": 100}},
        "accounts": [{"Basic": {"account_id": a, "cash_balance": 200 if a in (20,40) else 10000}} for a in (10,20,30,40)],
        "seed_orders": []}
    owner._request("POST", "/rooms", scenario)
    owner.add_member(args.room, "agent-riskqa", "trader")
    owner.assign_account(args.room, 40, "agent-riskqa")
    owner.place(args.room, 10, "Sell", 101, 100)
    owner.place(args.room, 30, "Buy", 99, 100)
    for i in range(8):
        owner.place(args.room, 30 if i % 2 == 0 else 10, "Buy" if i % 2 == 0 else "Sell", 101 if i % 2 == 0 else 99, 1)
        owner.advance_clock(args.room, 1)
    service = TradingService(output / "service", None, args.base_url)
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler(service, secrets.token_urlsafe(32), set()))
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
    token_path = output / "tool.token"
    try:
        service.create({"id": "riskqa", "room": args.room, "account_id": 40, "instruments": [instrument], "orders_per_minute": 100})
        token = service.issue_access("riskqa")["token"]; token_path.write_text(token)
        tool_url = f"http://127.0.0.1:{server.server_port}"
        client = ServiceClient(tool_url, "riskqa", token)
        service.start("riskqa")
        context = client.call("context", {})
        decision = client.call("decision_begin", {"generation": context["generation"], "plan": "Disposable acceptance"})
        fence = {"generation": decision["generation"], "decision_id": decision["decision_id"]}
        entry = client.call("trade", {**fence, "request_id": "qa-entry", "instrument": instrument, "action": "bracket", "side": "Buy", "qty": 5,
            "execution_mode": "unbounded", "take_profit_tick": 110, "stop_loss_tick": 90})
        assert entry["accepted"], entry
        observation = client.call("market_read", {"instrument": instrument, "kind": "observe"})
        assert observation["position_protections"][0]["status"] == "armed", observation
        assert observation["own_account"]["Perp"]["position_qty"] == 5
        client.call("trade", {**fence, "request_id": "qa-remove", "instrument": instrument, "action": "protection"})
        count = asyncio.run(probe_mcp(tool_url, token_path, output / "mcp-chart.png"))
        raw_chart = owner.chart_export(args.room, instrument, period=3, width=800, height=400)
        assert raw_chart["bar_count"] >= 8
        for mark in (65, 60):
            owner._request("POST", f"/rooms/{args.room}/instruments/{instrument}/mark-price", {"price_tick": mark})
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            interrupts = client.call("context", {})["interrupts"]
            if any(x["evidence"]["event"]["type"] == "PerpLiquidationSettled" for x in interrupts): break
            time.sleep(.1)
        else: raise AssertionError("liquidation did not reach the alert wakeup path")
        events = client.call("risk_events", {"instrument": instrument, "from_start": True})
        assert any(e["event"]["type"] == "PerpLiquidationSettled" for e in events["events"])
        assert all(e["event"]["account_id"] == 40 for e in events["events"])
        try: Client(args.base_url, user_id="agent-riskqa").risk_events(args.room, instrument, 20)
        except MarketForgeError as exc: assert exc.status == 403
        else: raise AssertionError("risk history escaped account scope")
        owner._request("POST", f"/rooms/{args.room}/instruments/{instrument}/mark-price", {"price_tick": 100})
        if args.strategy:
            assert service.sandbox.check()["available"], "enable the existing Docker sandbox first"
            context = client.call("context", {})
            decision = client.call("decision_begin", {"generation": context["generation"], "plan": "Test isolated Python on actual market history"})
            fence = {"generation": decision["generation"], "decision_id": decision["decision_id"]}
            code = '''import os
def decide(observations, state):
    assert os.getuid() != 0
    view = observations["V-BTC-PERP"]
    assert len(view["analysis_data"]["candles"]["candles"]) >= 8
    assert view["analysis_data"]["indicators"]["latest"] is not None
    assert "margin_buffer" in view["risk"]
    if state.get("done"): return {"actions": [], "state": state}
    return {"actions": [{"instrument":"V-BTC-PERP","action":"bracket","side":"Buy","qty":1,
        "execution_mode":"unbounded","take_profit_tick":110,"stop_loss_tick":90}], "state":{"done":True}}
'''
            saved = client.call("strategy_save", {**fence, "request_id": "qa-save", "name": "native", "code": code,
                "interval_seconds": 2, "market_data": {"interval_ms": 1000, "limit": 50}})
            tested = client.call("strategy_test", {**fence, "request_id": "qa-test", "name": "native"})
            assert tested["orders_submitted"] is False and tested["result"]["actions"], tested
            client.call("strategy_start", {**fence, "request_id": "qa-start", "name": "native"})
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                view = client.call("market_read", {"instrument": instrument, "kind": "observe"})
                if view["own_account"]["Perp"]["position_qty"] == 1: break
                time.sleep(.1)
            else: raise AssertionError("isolated strategy did not submit its bracket entry")
            assert view["position_protections"][0]["status"] == "armed", view
            client.call("strategy_stop", {**fence, "request_id": "qa-stop", "name": "native", "cancel_orders": False})
            (output / "strategy.json").write_text(json.dumps({"version": saved["version"], "sandbox": service.sandbox.check(),
                "strategy_state": service.strategies("riskqa")[0]["state"], "observation": view}, indent=2), encoding="utf-8")
            owner.protect_position(args.room, instrument, 40, idempotency_key="qa-strategy-clear")
            owner._request("POST", f"/rooms/{args.room}/instruments/{instrument}/orders", {"participant_id":"qa-cleanup", "account_id":40,
                "action":{"PlaceReduceOnlyMarket":{"side":"Sell","qty":1}}})
        # Leave a human account with native protection armed for restart/UI QA.
        human = owner.place_bracket(args.room, instrument, 20, "Buy", 5, take_profit_tick=110, stop_loss_tick=50, idempotency_key="human-qa-entry")
        assert human["accepted"], human
        result = {"room": args.room, "tool_count": count, "native_mcp_image": True, "risk_events": events,
            "liquidation_wakeup": True, "private_access_denied": True, "human_position_qty": 5, "isolated_strategy": args.strategy}
        (output / "live.json").write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8")
        print(json.dumps({k:v for k,v in result.items() if k != "risk_events"}))
    finally:
        for trader in service.store.all("trader"): service.stop(trader["id"])
        for _, threads in service.workers.values():
            for worker in threads: worker.join(3)
        server.shutdown(); server.server_close(); thread.join(3)
        service.store.db.close()
        token_path.unlink(missing_ok=True)


if __name__ == "__main__": main()
