"""Real exchange + HTTP service + stdio MCP event acceptance; no model calls."""
import argparse
import asyncio
import copy
import json
import os
from pathlib import Path
import tempfile
import threading
from http.server import ThreadingHTTPServer

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from marketforge.agents.__main__ import handler
from marketforge.agents.replay import isolated_exchange
from marketforge.agents.runtime import TradingService

ROOT = Path(__file__).resolve().parents[1]


def scenarios():
    base = json.loads((ROOT / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
    instrument = copy.deepcopy(base["market"]["Spot"]["instrument"])
    instrument.update(instrument_id="V-BTC-PERP", symbol="V-BTC-PERP")
    perp = {"instrument": instrument, "clearing": {"leverage": 10, "maker_fee_ppm": 0, "taker_fee_ppm": 0,
        "maintenance_margin_ppm": 50000, "liquidation_fee_ppm": 10000}, "risk": {}, "initial_mark_price_tick": 100}
    liquidation = {"room_id": "event-liquidation", "market": {"Perp": perp}, "seed_orders": [],
        "accounts": [{"Basic": {"account_id": a, "cash_balance": c}} for a, c in [(10, 10000), (20, 200), (30, 10000)]]}
    funding = copy.deepcopy(base)
    funding["room_id"] = "event-funding"
    for command in funding["seed_orders"]:
        order = command["NewOrder"]
        order["kind"]["Limit"]["price_tick"] = 9999 if order["side"] == "Buy" else 10001
    funding["accounts"] = [{"Spot": {"account_id": 10, "cash_balance": 10000000, "position_qty": 50}},
        {"Basic": {"account_id": 20, "cash_balance": 100000}}]
    funded = copy.deepcopy(perp)
    funded.update(initial_mark_price_tick=10000, price_link={"spot_instrument_id": "V-BTC-SPOT", "max_age_ms": 20000},
        funding={"interval_ms": 2000, "base_rate_ppm": -10000, "max_rate_ppm": 200000, "min_coverage_ppm": 900000})
    funding["extra_markets"] = [{"Perp": funded}]
    return liquidation, funding


async def validate(executable):
    report = {"provider_inference": False, "checks": []}
    with isolated_exchange(executable) as admin, tempfile.TemporaryDirectory(prefix="marketforge-event-tools-") as folder:
        liquidation, funding = scenarios()
        for scenario in (liquidation, funding):
            admin._request("POST", "/rooms", {"scenario": scenario, "autostart_agents": False})
        def order(room, account, side, price, qty):
            receipt = admin._request("POST", f"/rooms/{room}/instruments/V-BTC-PERP/orders",
                {"participant_id": "event-controller", "account_id": account,
                 "action": {"PlaceLimit": {"side": side, "price_tick": price, "qty": qty}}})
            assert receipt["accepted"], receipt
        for account, side, price, qty in [(10, "Sell", 100, 10), (20, "Buy", 100, 10), (30, "Buy", 70, 4)]:
            order("event-liquidation", account, side, price, qty)
        for account, side, price, qty in [(10, "Sell", 10000, 1), (20, "Buy", 10000, 1), (10, "Buy", 9999, 10), (10, "Sell", 10001, 10)]:
            order("event-funding", account, side, price, qty)
        service = TradingService(folder, None, admin.base_url)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler(service, "event-validation-operator-token", set()))
        server.daemon_threads = True
        serving = threading.Thread(target=server.serve_forever); serving.start()
        try:
            for trader, room in [("liquidation", "event-liquidation"), ("funding", "event-funding")]:
                admin.add_member(room, "agent-" + trader, "trader")
                admin.assign_account(room, 10, "agent-" + trader)
                service.create({"id": trader, "room": room, "account_id": 10, "instruments": ["V-BTC-PERP"], "backend": "external"})
                token = service.issue_access(trader)["token"]
                service.start(trader)
                params = StdioServerParameters(command=os.sys.executable,
                    args=["-m", "marketforge.agents.mcp_server", "--service-url", f"http://127.0.0.1:{server.server_port}", "--trader", trader],
                    env={"PYTHONPATH": str(ROOT / "python"), "MARKETFORGE_TOOL_TOKEN": token})
                async with stdio_client(params) as (read, write):
                    async with ClientSession(read, write) as session:
                        await session.initialize()
                        async def call(name, args):
                            result = await session.call_tool(name, args)
                            assert not result.isError, result.structuredContent
                            return result.structuredContent
                        schemas = await session.list_tools()
                        assert {"market_events", "event_alert_set"} <= {t.name for t in schemas.tools}
                        context = await call("context", {})
                        decision = await call("decision_begin", {"generation": context["generation"], "plan": "watch public events"})
                        control = {"decision_id": decision["decision_id"], "generation": decision["generation"]}
                        event_filter = {"event_type": trader, **({"min_notional": "280", "side": "Sell"} if trader == "liquidation" else {"max_rate_ppm": -10000})}
                        await call("event_alert_set", {**control, "request_id": "rule-001", "name": "event-watch", "instrument": "V-BTC-PERP", **event_filter})
                        if trader == "liquidation":
                            admin._request("POST", f"/rooms/{room}/instruments/V-BTC-PERP/mark-price", {"price_tick": 70})
                        else:
                            admin.advance_clock(room, 2)
                        for _ in range(100):
                            context = await call("context", {})
                            if any(t["name"] == "event-watch" for t in context["interrupts"]): break
                            await asyncio.sleep(.05)
                        trigger = next(t for t in context["interrupts"] if t["name"] == "event-watch")
                        assert trigger["event_count"] == 1, trigger
                        assert context["generation"] > control["generation"]
                        history = await call("market_events", {"instrument": "V-BTC-PERP", "from_start": True, **event_filter})
                        assert len(history["events"]) == 1, history
                        assert history["events"] == trigger["evidence"], (history, trigger)
                        assert "account_id" not in json.dumps(history)
                        stale = await session.call_tool("event_alert_set", {**control, "request_id": "stale-001", "name": "old-plan", "instrument": "V-BTC-PERP", **event_filter})
                        assert stale.isError and "stale" in stale.structuredContent["error"]
                        report["checks"].append({"event_type": trader, "event": history["events"][0],
                            "trigger_count": trigger["event_count"], "generation": context["generation"],
                            "stale_decision_rejected": True, "tool_count": len(schemas.tools)})
                service.stop(trader)
        finally:
            for trader in service.store.all("trader"): service.stop(trader["id"])
            for _, threads in service.workers.values():
                for thread in threads: thread.join(3)
            server.shutdown(); server.server_close(); serving.join(2)
            service.store.db.close()
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exchange-server", default=str(ROOT / "target/debug/exchange-server.exe"))
    parser.add_argument("--report", required=True)
    args = parser.parse_args()
    result = asyncio.run(validate(args.exchange_server))
    Path(args.report).write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"passed": len(result["checks"]), "provider_inference": False}))
