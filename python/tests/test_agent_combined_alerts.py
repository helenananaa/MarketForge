import copy
import threading
import unittest
from unittest.mock import patch

from marketforge import Client
from marketforge.agents.runtime import Runtime
from marketforge.agents.combined_alerts import numeric
from python.tests import test_agent_runtime as fixture


class CombinedAlertsTests(unittest.TestCase):
    setUp = fixture.RuntimeTests.setUp
    tearDown = fixture.RuntimeTests.tearDown
    call = fixture.RuntimeTests.call

    def install(self):
        self.stats = {"market_time_ms":2000,"end_market_time_ms":2000,"through_command_seq":7,
            "complete":True, "status":"Running", "metrics":{"long_liquidation_notional":"10000000000000000001", "delta_qty":"-4",
            "vwap_tick":{"numerator":"301","denominator":"3"}}}
        self.price, self.queries = 70, []
        def observation(config, instrument):
            return {"market_time_ms":2000, "step":2, "status":"Running", "public_trades":[{"price_tick":self.price}]}
        self.runtime.observation = observation
        def request(method, path, *args, **kwargs):
            self.queries.append((path, copy.deepcopy(kwargs.get("query"))))
            if path.endswith(("/window-stats", "/order-flow")): return copy.deepcopy(self.stats)
            return {"market_time_ms":2000, "candles":[{"open_time_ms":i*1000,"close_tick":p,"high_tick":p,"low_tick":p,"volume":1} for i,p in enumerate([100,110,90])]}
        self.exchange._request = request
        config = self.runtime.config("alice"); config["status"] = "running"
        self.runtime.store.put("trader","alice",config)

    def rule(self, source="window", metric="long_liquidation_notional", right="10000000000000000001", **options):
        return {"left":{"source":source,"instrument":"PERP","metric":metric,**options}, "op":"gte", "right":right}

    def alert(self, **options):
        return {"name":"compound", "rules":[self.rule(window_ms=30000), self.rule("state","last_price",80) | {"op":"lt"}], **options}

    def poll(self):
        self.runtime.alerts.combined_checks.clear()
        self.runtime.alerts.poll_combined("alice")

    def test_exact_sum_and_state_combination_interrupts_only_when_both_match(self):
        self.install(); self.price=90
        self.call("set","combined_alert_set",self.alert())
        self.runtime.alerts.poll("alice")  # Empty state conditions must never trigger.
        self.poll(); self.assertEqual(self.runtime.alerts.state("alice")["generation"],0)
        token=self.runtime.alerts.token("alice",threading.Event())
        self.price=70; self.poll()
        state=self.runtime.alerts.state("alice")
        self.assertEqual(state["generation"],1); self.assertTrue(token.is_set())
        self.assertEqual(state["pending"][0]["conditions"][0]["left"]["value"],"10000000000000000001")
        self.poll(); self.assertEqual(self.runtime.alerts.state("alice")["generation"],1)

    def test_partial_unknown_time_skew_and_network_failure_break_sustain(self):
        self.install(); self.call("set","combined_alert_set",self.alert(sustain_seconds=2))
        with patch('marketforge.agents.combined_alerts.time.time', return_value=100): self.poll()
        self.assertEqual(self.runtime.alerts.list("alice")[0]["candidate_since"],100)
        for change in ({"complete":False},{"market_time_ms":3000,"end_market_time_ms":3000},{"status":"Stopped"}):
            before=copy.deepcopy(self.stats); self.stats.update(change)
            with patch('marketforge.agents.combined_alerts.time.time', return_value=105): self.poll()
            self.assertIsNone(self.runtime.alerts.list("alice")[0]["candidate_since"])
            self.assertEqual(self.runtime.alerts.state("alice")["generation"],0)
            self.stats=before
        with patch.object(self.runtime,'read_window_stats', side_effect=TimeoutError()): self.poll()
        self.assertEqual(self.runtime.alerts.list("alice")[0]["evaluation_error"],'TimeoutError')
        with patch('marketforge.agents.combined_alerts.time.time', return_value=106): self.poll()
        with patch('marketforge.agents.combined_alerts.time.time', return_value=108): self.poll()
        self.assertEqual(self.runtime.alerts.state("alice")["generation"],1)

    def test_repeat_rearm_hysteresis_cooldown_unknown_cannot_rearm(self):
        self.install(); args=self.alert(rules=[self.rule("state","last_price",80)],repeat=True,hysteresis=5,cooldown_seconds=2)
        self.call("set","combined_alert_set",args); self.price=90
        with patch('marketforge.agents.combined_alerts.time.time',return_value=100): self.poll()
        self.price=77
        with patch('marketforge.agents.combined_alerts.time.time',return_value=103): self.poll()
        self.assertTrue(self.runtime.alerts.list("alice")[0]["latched"])
        self.price=74; self.poll(); self.assertFalse(self.runtime.alerts.list("alice")[0]["latched"])
        self.price=90
        with patch('marketforge.agents.combined_alerts.time.time',return_value=103): self.poll()
        self.assertEqual(self.runtime.alerts.list("alice")[0]["trigger_count"],2)
        self.runtime.observation=lambda *_:{"market_time_ms":2000,"status":"Running"}
        self.poll(); self.assertTrue(self.runtime.alerts.list("alice")[0]["latched"])

    def test_indicator_operand_rational_vwap_any_and_warmup(self):
        self.install()
        rhs={"source":"indicator","instrument":"PERP","metric":"sma","period":2}
        self.call("set","combined_alert_set",self.alert(rules=[self.rule("state","last_price",rhs)|{"op":"lt"}, self.rule(metric="vwap_tick",right="100.33333333333333333333333333333334")],match="any"))
        self.poll(); self.assertEqual(self.runtime.alerts.state("alice")["generation"],1)
        self.assertLess(numeric("100.33333333333333333333333333333333"), numeric(self.stats["metrics"]["vwap_tick"]))
        self.call("warmup","combined_alert_set",self.alert(rules=[self.rule("indicator","rsi",50,period=14)]))
        self.poll(); self.assertEqual(self.runtime.alerts.list("alice")[-1]["status"],"armed")

    def test_validation_scope_and_read_sdk(self):
        self.install()
        bad=[self.alert(rules=[]),self.alert(rules=[self.rule(window_ms=0)]),self.alert(rules=[self.rule("state","delta_qty")]),
            self.alert(rules=[self.rule(right=True)]),self.alert(rules=[self.rule(right="NaN")]),self.alert(rules=[self.rule(right="1/3")]),
            self.alert(rules=[self.rule("indicator","rsi",50,period=1)]),self.alert(max_skew_market_time_ms=-1),
            self.alert(priority="normal",pause_strategies=True),self.alert(evaluation_interval_seconds=0)]
        for i,args in enumerate(bad): self.assertIn("error",self.call(str(i),"combined_alert_set",args))
        foreign=self.alert(); foreign["rules"][0]["left"]["instrument"]="OTHER"
        self.assertIn("error",self.call("scope","combined_alert_set",foreign))
        for name in ('window_stats','order_flow'):
            self.assertEqual(self.call(name,name,{"instrument":"PERP","large_trade_min_notional":"10000000000000000001"})['metrics'],self.stats['metrics'])
            self.assertEqual(self.queries[-1][1]['large_trade_min_notional'],'10000000000000000001')
        client=Client('http://unused'); client._request=self.exchange._request
        client.window_stats('room /','PERP /'); self.assertIn('room%20%2F',self.queries[-1][0])
        client.order_flow('room','PERP'); self.assertTrue(self.queries[-1][0].endswith('/order-flow'))

    def test_caching_rate_limits_replacement_and_restart(self):
        self.install(); self.price=90
        first=self.call("set","combined_alert_set",self.alert())
        self.call("set2","combined_alert_set",self.alert(name="second"))
        self.poll(); self.assertEqual(len(self.queries),1)
        self.runtime.alerts.poll_combined("alice"); self.assertEqual(len(self.queries),1)
        self.runtime.store.db.close()
        self.runtime=Runtime(self.directory.name,fixture.PLUGIN,"http://unused",self.sandbox,lambda _:self.exchange)
        self.assertEqual(self.runtime.alerts.list("alice")[0]["rules"],first["rules"])
        config=self.runtime.config('alice'); config['status']='running'; self.runtime.store.put('trader','alice',config)
        self.runtime.observation=lambda *_:{'market_time_ms':2000,'status':'Running','public_trades':[{'price_tick':70}]}
        self.runtime.alerts.poll_combined('alice'); self.assertEqual(self.runtime.alerts.state('alice')['generation'],1)

    def test_cancellation_during_network_read_discards_stale_evaluation(self):
        self.install(); self.call('set','combined_alert_set',self.alert())
        original=self.runtime.read_window_stats
        def read(*args):
            with self.runtime.lock('alice'): self.runtime.alerts.cancel('alice','compound')
            return original(*args)
        self.runtime.read_window_stats=read
        self.poll(); self.assertEqual(self.runtime.alerts.state('alice')['generation'],0)

    def test_normal_priority_bounded_skew_queues_without_fencing_current_work(self):
        self.install(); self.stats.update(market_time_ms=2100,end_market_time_ms=2100)
        config=self.runtime.config('alice'); config['backend']='external'; self.runtime.store.put('trader','alice',config)
        self.runtime.store.put('external_control','alice',{'decision_id':'active-plan','generation':0,'scheduled_wake':None})
        token=self.runtime.alerts.token('alice',threading.Event())
        self.call('normal','combined_alert_set',self.alert(priority='normal',max_skew_market_time_ms=100))
        self.poll()
        state=self.runtime.alerts.state('alice')
        self.assertEqual(state['generation'],0); self.assertFalse(token.is_set())
        self.assertEqual(state['queued'][0]['kind'],'combined')
        self.assertEqual(self.runtime.control('alice')['decision_id'],'active-plan')


if __name__ == '__main__': unittest.main()
