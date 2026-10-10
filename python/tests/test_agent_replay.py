import json
import os
from pathlib import Path
import tempfile
import time
import unittest

from marketforge.agents.replay import Recorder, isolated_exchange, replay
from marketforge.agents.runtime import TradingService


ROOT = Path(__file__).resolve().parents[2]


class ReplayTests(unittest.TestCase):
    def test_recording_references_observed_decisions_and_keeps_original_request_identity(self):
        recorder = Recorder({}, {})
        recorder.tool("decision_begin", {"generation": 0}, {"decision_id": "opaque", "generation": 0, "observations": {}})
        recorder.tool("trade", {"decision_id": "opaque", "request_id": "same-order", "generation": 0}, {"accepted": True})
        self.assertEqual(recorder.trace["steps"][1]["arguments"]["decision_id"], "decision-0")
        self.assertEqual(recorder.trace["steps"][1]["arguments"]["request_id"], "same-order")
        self.assertNotIn("opaque", json.dumps(recorder.trace))
        with self.assertRaises(ValueError):
            recorder.tool("strategy_analyze", {}, {})

    @unittest.skipUnless(os.environ.get("MARKETFORGE_REPLAY_EXCHANGE_TEST") == "1", "set MARKETFORGE_REPLAY_EXCHANGE_TEST=1 for isolated real exchange replay")
    def test_two_real_exchange_replays_match_alert_rejection_fill_and_duplicate_receipt(self):
        executable = ROOT / "target/debug/exchange-server.exe"
        scenario = json.loads((ROOT / "scripts/fixtures/f6_batch_spec.json").read_text())["scenario"]
        room, instrument = scenario["room_id"], scenario["market"]["Spot"]["instrument"]["instrument_id"]
        config = {"id": "replay", "room": room, "account_id": 20, "instruments": [instrument], "backend": "external"}
        recorder = Recorder(scenario, config)
        with isolated_exchange(executable) as admin, tempfile.TemporaryDirectory() as folder:
            admin._request("POST", "/rooms", {"scenario": scenario, "autostart_agents": False})
            admin.add_member(room, "agent-replay", "trader"); admin.assign_account(room, 20, "agent-replay")
            service = TradingService(folder, None, admin.base_url)
            current = service.create(config); current["status"] = "running"; service.store.put("trader", "replay", current)
            def call(name, args):
                try:
                    result = service.external_call("replay", name, args)
                except ValueError as exc:
                    recorder.tool(name, args, error=exc)
                    return None
                recorder.tool(name, args, result)
                return result
            try:
                old = call("decision_begin", {"generation": 0, "plan": "buy at the current ask"})
                control = {"decision_id": old["decision_id"], "generation": 0}
                call("alert_set", {**control, "request_id": "alert", "name": "ask-changed", "conditions": [
                    {"instrument": instrument, "metric": "best_ask", "op": "gte", "value": 105}]})
                for index, action in enumerate(({"Cancel": {"order_id": 2}}, {"PlaceLimit": {"side": "Sell", "price_tick": 105, "qty": 8}})):
                    command = {"participant_id": "controller", "account_id": 10, "action": action}
                    admin._request("POST", f"/rooms/{room}/instruments/{instrument}/orders", command, idempotency_key=f"shock-{index}")
                    recorder.market(instrument, command)
                admin.advance_clock(room, 3); recorder.clock(3)
                service.alerts.poll("replay"); recorder.poll(1)
                buy = {**control, "request_id": "purchase", "instrument": instrument, "action": "ioc", "side": "Buy", "price_tick": 105, "qty": 1}
                self.assertIsNone(call("trade", buy))
                fresh = call("decision_begin", {"generation": 1, "plan": "continue at 105"})
                buy.update(decision_id=fresh["decision_id"], generation=1)
                self.assertTrue(call("trade", buy)["accepted"])
                self.assertTrue(call("trade", buy)["accepted"])
            finally:
                service.store.db.close()
        runs = [replay(recorder.trace, executable) for _ in range(2)]
        self.assertEqual(runs[0]["digest"], runs[1]["digest"])
        account = runs[0]["final_observations"][instrument]["own_account"]["Spot"]
        self.assertEqual(account["position_qty"], 1)
        self.assertEqual(account["cash_balance"], 100000 - 105)
        self.assertFalse(runs[0]["provider_inference"])


if __name__ == "__main__":
    unittest.main()
