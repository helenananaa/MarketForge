import asyncio
import base64
import copy
import struct
import unittest

from marketforge.agents.market_data import indicators, render_chart
from marketforge.agents.alerts import metric_value
from marketforge.agents.mcp_server import build_server
from python.tests import test_agent_runtime as fixture


def candles():
    return [{"open_time_ms": i * 1000, "open_tick": 100 + i,
             "high_tick": 102 + i, "low_tick": 99 + i, "close_tick": 101 + i,
             "volume": 2, "quote_volume": (101 + i) * 2} for i in range(20)]


def data():
    return {"room_id": "room", "instrument_id": "PERP", "interval_ms": 1000,
            "market_time_ms": 20000, "candles": candles()}


class MarketDataTests(unittest.TestCase):
    def test_indicator_warmup_and_known_series(self):
        study = indicators(candles(), 3)
        self.assertIsNone(study["rows"][1]["sma"])
        self.assertEqual(study["rows"][2]["sma"], 102)
        self.assertIsNone(study["rows"][2]["rsi"])
        self.assertEqual(study["rows"][3]["rsi"], 100)
        self.assertEqual(study["latest"]["atr"], 3)
        self.assertEqual(study["latest"]["vwap"], 110.5)
        self.assertIsNone(indicators([])["latest"])

    def test_chart_is_decodable_png_and_bounded(self):
        chart = render_chart(data(), indicators(candles(), 3), 800, 400)
        raw = base64.b64decode(chart["image_base64"], validate=True)
        self.assertEqual(raw[:8], b"\x89PNG\r\n\x1a\n")
        self.assertEqual(struct.unpack(">II", raw[16:24]), (800, 400))
        with self.assertRaises(ValueError):
            render_chart(data(), indicators(candles()), 8000, 400)
        huge = data(); huge["candles"][0]["volume"] = 2**54
        with self.assertRaises(ValueError):
            render_chart(huge, indicators(huge["candles"]))

    def test_mcp_emits_native_image_without_base64_text(self):
        from mcp import types
        chart = render_chart(data(), indicators(candles()), 400, 300)
        class Service:
            def request(self, endpoint):
                from marketforge.agents.runtime import TOOLS
                return TOOLS
            def call(self, name, args): return chart
        server = build_server(Service())
        request = types.CallToolRequest(method="tools/call", params=types.CallToolRequestParams(name="chart_export", arguments={"instrument": "PERP"}))
        result = asyncio.run(server.request_handlers[types.CallToolRequest](request)).root
        self.assertIsInstance(result.content[0], types.ImageContent)
        self.assertNotIn("image_base64", result.structuredContent)
        self.assertNotIn(chart["image_base64"], result.content[1].text)

    def test_risk_metrics_use_private_snapshot(self):
        view = {"risk": {"mark_price_tick": 101, "margin_buffer": -2, "margin_ratio_ppm": 1200000},
                "own_account": {"Perp": {"maintenance_margin": 10, "margin_status": "liquidatable"}}}
        self.assertEqual(metric_value(view, "margin_buffer"), -2)
        self.assertEqual(metric_value(view, "liquidatable"), 1)
        self.assertIsNone(metric_value({}, "liquidatable"))


class MarketRuntimeTests(unittest.TestCase):
    setUp = fixture.RuntimeTests.setUp
    tearDown = fixture.RuntimeTests.tearDown
    call = fixture.RuntimeTests.call

    def install_data(self):
        self.requests = []
        original = self.exchange._request
        def request(method, path, *args, **kwargs):
            self.requests.append((path, kwargs.get("query")))
            if path.endswith("/candles"): return data()
            return original(method, path, *args, **kwargs)
        self.exchange._request = request

    def test_market_scope_history_cursors_and_strategy_injection(self):
        self.install_data()
        result = self.call("history", "market_history", {"instrument": "PERP", "limit": 2000, "before_open_time_ms": 15000})
        self.assertEqual(len(result["candles"]), 20)
        self.assertEqual(self.requests[-1][1]["before_open_time_ms"], 15000)
        self.assertIn("error", self.call("foreign", "market_history", {"instrument": "OTHER"}))
        self.assertIn("error", self.call("too-big", "market_history", {"instrument": "PERP", "limit": 2001}))
        saved = self.call("save-bars", "strategy_save", {"name": "bars", "code": "def decide(observations, state): return {}",
            "interval_seconds": 2, "market_data": {"interval_ms": 1000, "limit": 50}})
        observed = self.runtime.strategy_observations(self.runtime.config("alice"), saved)
        self.assertEqual(observed["PERP"]["analysis_data"]["candles"]["candles"], candles())
        patched = self.call("patch-bars", "strategy_patch", {"name": "bars", "files": {"extra.py": "x=1"}})
        self.assertEqual(patched["market_data"], saved["market_data"])

    def test_bracket_explicit_market_mode_and_protection_removal(self):
        args = {"instrument": "PERP", "action": "bracket", "side": "Buy", "qty": 2, "take_profit_tick": 110, "stop_loss_tick": 90}
        self.assertIn("error", self.call("no-mode", "trade", args))
        self.assertTrue(self.call("entry", "trade", {**args, "execution_mode": "unbounded"})["accepted"])
        self.assertIn("PlaceBracket", self.exchange.orders[-1]["action"])
        self.call("remove", "trade", {"instrument": "PERP", "action": "protection"})
        self.assertIsNone(self.exchange.orders[-1]["action"]["SetPositionProtection"]["protection"])

    def test_risk_cursor_survives_and_does_not_duplicate_interrupts(self):
        config = self.runtime.config("alice"); config["status"] = "running"; self.runtime.store.put("trader", "alice", config)
        calls = []
        def request(config, args):
            calls.append(copy.deepcopy(args))
            first = "after_command_seq" not in args
            return {"events": [{"command_seq": 7, "market_time_ms": 1000, "event": {"type": "PerpLiquidationSettled", "account_id": 20}}] if first else [],
                    "next_after_command_seq": 7}
        self.runtime.read_risk_events = request
        self.runtime.poll_risk_events("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 2)
        self.runtime.poll_risk_events("alice")
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 2)
        self.assertEqual(calls[-1]["after_command_seq"], 7)
        config["status"] = "paused"; self.runtime.store.put("trader", "alice", config)
        self.runtime.notify_risk("alice", "late", {})
        self.assertEqual(self.runtime.alerts.state("alice")["generation"], 2)
