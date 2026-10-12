"""Qualified direct Rust decisions survive durable PostgreSQL restart."""
import json
import os
import time
import unittest

import test_pine_http as http
import test_pine_broker_http as broker_http


class PineInProcessRecoveryTests(unittest.TestCase):
    setUpClass = classmethod(http.PineHttpTests.setUpClass.__func__)
    setUp = broker_http.PineBrokerHttpTests.setUp
    start = broker_http.PineBrokerHttpTests.start
    stop = broker_http.PineBrokerHttpTests.stop
    tick = broker_http.PineBrokerHttpTests.tick
    observation = broker_http.PineBrokerHttpTests.observation
    room = broker_http.PineBrokerHttpTests.room
    data = broker_http.PineBrokerHttpTests.data

    def test_qualified_limit_postgres_restart_and_cancel_confirmation(self):
        dsn = os.environ.get("MARKETFORGE_TEST_DATABASE_URL")
        if not dsn:
            if os.environ.get("MARKETFORGE_REQUIRE_POSTGRES_TESTS") == "1":
                self.fail("direct Rust recovery requires a test database")
            self.skipTest("PostgreSQL test database not set")
        self.env.update(MARKETFORGE_DATABASE_URL=dsn, MARKETFORGE_PINE_IN_PROCESS="1")
        self.start()
        room = f"rust-pine-restart-{time.time_ns()}"
        self.room(room, "limit.pine")
        for index, price in enumerate((104, 103, 102, 101, 100)):
            saved = self.tick(room, price, index)
        order = self.observation(room, 300)["own_orders"][0]
        self.assertEqual((order["price_tick"], order["remaining_qty"]), (98, 2))
        confirmed = self.client.step_bots(room, "rust-confirm-limit")
        durable = self.data(confirmed)
        self.stop()
        self.start()
        self.assertEqual(self.observation(room, 300)["own_orders"], [order])
        self.assertEqual(self.client.step_bots(room, f"{room}-step-4"), saved)
        resumed = self.client.step_bots(room, "rust-restart-observe")
        self.assertEqual(self.observation(room, 300)["own_orders"], [order])
        self.assertEqual(self.data(resumed)["orders"], durable["orders"])
        cancelled = self.tick(room, 110, 5)
        self.assertEqual(self.observation(room, 300)["own_orders"], [])
        self.assertEqual(self.client.step_bots(room, f"{room}-step-5"), cancelled)
        # Metrics is text in the HTTP SDK; fetch directly to verify actual route.
        import urllib.request
        request = urllib.request.Request(self.base + "/metrics", headers={"Authorization":"Bearer pine-test"})
        with urllib.request.urlopen(request, timeout=10) as response:
            metrics = response.read().decode()
        self.assertRegex(metrics, r'marketforge_bench_bot_inprocess_call_count_total [1-9]')
        self.assertRegex(metrics, r'marketforge_bench_bot_worker_acquire_count_total 0')
        (self.receipts / "rust-pine-postgres-restart.json").write_text(json.dumps({"order":order,
            "durable":durable, "restored":self.data(resumed), "cancelled":self.data(cancelled)}, indent=2), encoding="utf-8")


if __name__ == "__main__":
    unittest.main()
