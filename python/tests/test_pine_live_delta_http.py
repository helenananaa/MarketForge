"""Real auto-mode schema-3 Pine deltas recover from PostgreSQL and keep trading."""
import hashlib
import json
import os
import shutil
import subprocess
import time
import unittest
import urllib.request

import test_pine_http as http
import test_pine_broker_http as broker_http


class PineLiveDeltaRecoveryTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    setUp = broker_http.PineBrokerHttpTests.setUp
    start = broker_http.PineBrokerHttpTests.start
    stop = broker_http.PineBrokerHttpTests.stop
    observation = broker_http.PineBrokerHttpTests.observation
    room = broker_http.PineBrokerHttpTests.room
    data = broker_http.PineBrokerHttpTests.data

    def test_auto_schema3_delta_postgres_restart_preserves_state_and_continues(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("live schema3 recovery requires the test PostgreSQL database")
            self.skipTest("PostgreSQL test database not set")
        psql = os.environ.get("MARKETFORGE_TEST_PSQL") or shutil.which("psql")
        self.assertTrue(psql, "psql is required to verify actual durable Pine delta rows")
        self.env.update(MARKETFORGE_DATABASE_URL=dsn, MARKETFORGE_PINE_IN_PROCESS="1",
                        MARKETFORGE_PINE_VM_CHECKPOINT="1")
        room = f"vm-live-delta-{time.time_ns()}"
        receipt = {"room": room, "qualified": False,
                   "server_sha256": hashlib.sha256(self.binary.read_bytes()).hexdigest(),
                   "input_receipts": []}
        receipt_path = self.receipts / f"rust-pine-live-delta-postgres-{time.time_ns()}.json"
        self.addCleanup(lambda: receipt_path.write_text(json.dumps(receipt, indent=2), encoding="utf-8"))
        self.start()
        agents = self.room(room, "limit.pine", config={"history_limit":128}, manual=False)
        self.client.start_agents(room, agents, interval_ms=100)
        self.client._request("POST", f"/rooms/{room}/resume", {})

        def healthy():
            status = self.client._request("GET", f"/rooms/{room}/agents")
            self.assertFalse(status["bot_errors"], status)
            self.assertFalse(status.get("last_error"), status)
            return status

        def wait_for(predicate, message):
            deadline = time.monotonic() + 15
            while True:
                healthy()
                saved = self.client.room_bots(room)
                if predicate(saved):
                    return saved
                if time.monotonic() >= deadline:
                    self.fail(message)
                time.sleep(0.03)

        def feed(price):
            before = self.client.room_bots(room)
            previous = (self.data(before) or {}).get("evaluated_bars", 0)
            # Only real gateway trades create the new candle. No manual clock
            # step or bot step can replace the auto-mode decision/commit path.
            for account in (10, 20):
                for order in self.observation(room, account)["own_orders"]:
                    self.client.cancel(room, account, order["order_id"])
            sell = self.client.place(room, 10, "Sell", price, 1)
            buy = self.client.place(room, 20, "Buy", price, 1)
            self.assertTrue(sell["accepted"], sell)
            self.assertTrue(buy["accepted"], buy)
            self.assertGreater(buy["clearing_event_count"], 0, "gateway pair must produce a real fill")
            target_open = buy["market_time_ms"] // 1000 * 1000
            receipt["input_receipts"].append({"price":price, "sell":sell, "buy":buy})
            return wait_for(lambda state: (self.data(state) or {}).get("evaluated_bars", 0) > previous
                            and (self.data(state) or {}).get("last_bar_time", -1) >= target_open,
                            "auto bot did not durably evaluate the new closed bar")

        def quiesce():
            self.client.pause_room(room)
            previous = None
            unchanged = 0
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                healthy()
                current = self.client.room_bots(room)
                unchanged = unchanged + 1 if current == previous else 0
                if unchanged >= 3:
                    return current
                previous = current
                time.sleep(0.05)
            self.fail("paused scheduler did not reach a stable committed state")

        def journal_evidence():
            # The room is generated above; quote nevertheless so the SQL query
            # remains scoped to this acceptance run if naming ever changes.
            quoted = "'" + room.replace("'", "''") + "'"
            sql = f"""
                WITH records AS (
                    SELECT mutation_seq, command_cursor, mutation_kind, payload_json
                    FROM marketforge_room_mutations WHERE room_id={quoted}
                ), selected AS (
                    SELECT json_build_object('mutation_seq',mutation_seq,
                        'command_cursor',command_cursor,'delta',payload_json->'delta') AS row
                    FROM records
                    WHERE mutation_kind='scheduler_delta'
                      AND payload_json->'delta'->>'version'='2'
                      AND EXISTS (
                        SELECT 1 FROM jsonb_array_elements(payload_json->'delta'->'pine_changes') p
                        WHERE p->'state'->'metadata'->>'broker_schema'='3'
                          AND jsonb_array_length(p->'state'->'arrays'->'vm_tail'->'items')>0
                      )
                    ORDER BY mutation_seq
                ), full_states AS (
                    SELECT json_build_object('mutation_seq',mutation_seq,
                        'revision',payload_json->'state'->'revision',
                        'pine_evaluated_bars', (
                            SELECT max((agent->'kind_state'->'Plugin'->'data'->>'evaluated_bars')::bigint)
                            FROM jsonb_array_elements(payload_json->'state'->'agents') agent
                            WHERE agent->'kind_state'->'Plugin'->'data'->>'broker_schema'='3'
                        )) AS row
                    FROM records WHERE mutation_kind='scheduler_progress'
                    ORDER BY mutation_seq
                )
                SELECT json_build_object(
                    'head', (SELECT max(mutation_seq) FROM records),
                    'latest_full_scheduler', (SELECT max(mutation_seq) FROM records
                        WHERE mutation_kind='scheduler_progress'),
                    'schema3_append_rows', (SELECT coalesce(json_agg(row),'[]'::json) FROM selected),
                    'full_scheduler_rows', (SELECT coalesce(json_agg(row),'[]'::json) FROM full_states)
                )::text;
            """
            result = subprocess.run([psql, "--dbname", dsn, "-X", "-A", "-t", "-q",
                                     "-v", "ON_ERROR_STOP=1", "-c", sql], capture_output=True,
                                    text=True, encoding="utf-8", timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            return json.loads(result.stdout)

        def assert_append_rows(evidence):
            rows = evidence["schema3_append_rows"]
            self.assertTrue(rows, "PostgreSQL contains no schema3 VM-tail append")
            for row in rows:
                delta = row["delta"]
                self.assertEqual(delta["version"], 2)
                for change in delta["pine_changes"]:
                    patch = change["state"]
                    self.assertEqual(patch["metadata"]["broker_schema"], 3)
                    self.assertNotIn("vm_checkpoint", patch["metadata"])
                    self.assertEqual(set(patch["arrays"]), {"vm_tail", "vm_tail_intents"})
                    self.assertEqual(len(patch["base_sha256"]), 32)
                    self.assertEqual(len(patch["patch_sha256"]), 32)
            return rows

        for _ in range(40):
            saved = feed(100)
            if self.data(saved).get("evaluated_bars", 0) >= 35 and self.data(saved).get("vm_tail"):
                break
        else:
            self.fail("live input did not cross the first VM checkpoint")
        # A later full scheduler snapshot could otherwise conceal a broken
        # append on recovery. Require at least one actual tail append after it.
        for _ in range(8):
            before = quiesce()
            evidence = journal_evidence()
            rows = assert_append_rows(evidence)
            if any(row["mutation_seq"] > evidence["latest_full_scheduler"] for row in rows):
                break
            self.client._request("POST", f"/rooms/{room}/resume", {})
            feed(100)
        else:
            self.fail("recovery suffix did not contain a schema3 append after the last full state")
        self.assertIn("Auto", before["mode"])
        self.assertTrue(before["bots_enabled"])
        self.assertEqual(self.data(before)["broker_schema"], 3)
        self.assertIsNotNone(self.data(before)["vm_checkpoint"])
        self.assertTrue(self.data(before)["vm_tail"])
        view = self.observation(room, 300)
        before_clock = self.client.clock(room)
        receipt.update(before_scheduler=before, before_observation=view,
                       before_clock=before_clock, journal_before=evidence)
        self.stop()
        self.start()
        restored = self.client.room_bots(room)
        restored_view = self.observation(room, 300)
        receipt.update(restored_scheduler=restored, restored_observation=restored_view)
        self.assertEqual(restored, before, "all committed scheduler and Pine JSON must recover exactly")
        self.assertEqual(self.client.clock(room), before_clock)
        self.assertEqual(restored_view["own_orders"], view["own_orders"])
        self.assertEqual(restored_view["own_account"], view["own_account"])
        self.client._request("POST", f"/rooms/{room}/resume", {})
        advanced = feed(99)
        wait_for(lambda _: bool(self.observation(room, 300)["own_orders"]),
                 "restored live Pine did not submit its new legitimate limit order")
        continued = quiesce()
        continued_view = self.observation(room, 300)
        self.assertGreater(self.data(continued)["evaluated_bars"], self.data(before)["evaluated_bars"])
        self.assertGreaterEqual(self.data(continued)["evaluated_bars"], self.data(advanced)["evaluated_bars"])
        self.assertEqual([(o["price_tick"], o["remaining_qty"]) for o in continued_view["own_orders"]], [(97, 2)])
        receipt.update(first_continued_scheduler=continued, first_continued_observation=continued_view,
                       continuation_attempts=[])
        previous_head = evidence["head"]
        for attempt in range(8):
            after_evidence = journal_evidence()
            receipt["continuation_attempts"].append({"scheduler": continued,
                "observation": continued_view, "journal": after_evidence})
            receipt.update(continued_scheduler=continued, continued_observation=continued_view,
                           journal_after=after_evidence)
            if any(row["mutation_seq"] > evidence["head"]
                   for row in assert_append_rows(after_evidence)):
                break
            # Revision 100 (and periodic room snapshots) legitimately persists
            # the complete scheduler. Prove that this new bar was persisted in
            # such a full state, then generate another real bar until the next
            # non-checkpoint decision produces an actual PostgreSQL append.
            self.assertTrue(any(row["mutation_seq"] > previous_head
                                and row["pine_evaluated_bars"] is not None
                                and row["pine_evaluated_bars"] >= self.data(advanced)["evaluated_bars"]
                                for row in after_evidence["full_scheduler_rows"]),
                            "new closed bar has neither a durable VM-tail append nor a covering full state")
            self.assertLess(attempt, 7,
                            "new closed bars never produced a post-restart durable VM-tail append")
            previous_head = after_evidence["head"]
            self.client._request("POST", f"/rooms/{room}/resume", {})
            advanced = feed(99)
            continued = quiesce()
            continued_view = self.observation(room, 300)
            self.assertGreaterEqual(self.data(continued)["evaluated_bars"],
                                    self.data(advanced)["evaluated_bars"])
        request = urllib.request.Request(self.base + "/metrics", headers={"Authorization":"Bearer pine-test"})
        with urllib.request.urlopen(request, timeout=10) as response:
            metrics = response.read().decode()
        self.assertRegex(metrics, r'marketforge_bot_runtime_evaluation_total\{route="direct",mode="checkpoint_restore",reason="session_missing"\} [1-9]')
        self.assertNotRegex(metrics, r'marketforge_bot_runtime_route_total\{route="(?:process|blocked)"[^\n]*\} [1-9]')
        receipt.update(continued_scheduler=continued, continued_observation=continued_view,
                       journal_after=after_evidence, qualified=True)


if __name__ == "__main__":
    unittest.main()
