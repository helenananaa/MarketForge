import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0,str(ROOT/"scripts"))
from joint_market_calibration import joint_recipe, fit_score
from microstructure_market import microstructure_recipe
from market_arrival_feedback import apply_feedback
from compare_behavior_reference import aggregate_receipts
from marketforge.calibration import reference_profile
from test_market_calibration import source, DAY

KNOBS = dict(arrival_scale=1,width_scale=.2,depth_scale=8,levels=4,activity=.8,market_ratio=.9)


class JointCalibrationTests(unittest.TestCase):
    def test_aggregation_preserves_quantity_time_price_side_and_order_boundaries(self):
        def row(instrument, time, price, order, side="Buy"):
            return dict(instrument_id=instrument,time_ms=time,price_tick=price,
                        taker_order_id=order,taker_side=side,qty=2,trade_id=order)
        spot="V-BTC-SPOT";perp="V-BTC-PERP"
        rows=[row(spot,10,100,1),row(spot,10,100,1),row(spot,10,100,2),
              row(spot,11,100,2),row(spot,11,101,2),
              row(perp,10,100,1),row(perp,20,100,2),row(perp,100,100,3),
              row(perp,100,100,4,"Sell"),row(perp,100,101,5,"Sell")]
        grouped=aggregate_receipts(rows,require_order_ids=True)
        self.assertEqual([r["qty"] for r in grouped],[4,2,2,2,4,2,2,2])
        self.assertEqual(sum(r["qty"] for r in rows),sum(r["qty"] for r in grouped))
        self.assertEqual(sum(r["underlying_trades"] for r in grouped),len(rows))
        legacy=[{k:v for k,v in r.items() if k!="taker_order_id"} for r in rows]
        self.assertEqual(len(aggregate_receipts(legacy)),len(rows))
        with self.assertRaises(ValueError):aggregate_receipts(legacy,require_order_ids=True)

    def test_recipe_and_search_inputs_do_not_use_holdout_or_future_funding(self):
        with tempfile.TemporaryDirectory() as directory:
            profile=reference_profile(source(directory),DAY,120)
            other=copy.deepcopy(profile)
            other["holdout"]={"poison":"must never be accessed"}
            other["funding_day"][1]["rate"]="0.5"
            self.assertEqual(joint_recipe(profile,KNOBS),joint_recipe(other,KNOBS))
            first, _ = joint_recipe(profile, KNOBS)
            second, _ = joint_recipe(other, KNOBS)
            self.assertEqual(apply_feedback(first, profile["fit"]), apply_feedback(second, other["fit"]))
            recipe,mapping=joint_recipe(profile,KNOBS)
            self.assertFalse(mapping["used_holdout_to_fit"])
            identities=[(a["Plugin"]["participant"]["account_id"],a["Plugin"]["participant"]["instrument_id"]) for a in recipe["agents"]]
            self.assertEqual(len(identities),len(set(identities)))
            accounts=[next(iter(a.values())) for a in recipe["scenario"]["accounts"]]
            self.assertTrue(all(a["cash_balance"]>0 for a in accounts))
            self.assertEqual(len(accounts),len({a["account_id"] for a in accounts}))
            self.assertTrue(all(a["Plugin"]["seed"]<=2**53-1 for a in recipe["agents"]))
            self.assertEqual(recipe["scenario"]["extra_markets"][0]["Perp"]["clearing"]["leverage"],5)

    def test_sparse_or_missing_volatility_is_penalized_in_fit_objective(self):
        with tempfile.TemporaryDirectory() as directory:
            fit=reference_profile(source(directory),DAY,120)["fit"]
            self.assertAlmostEqual(fit_score(fit,fit),0)
            sparse=copy.deepcopy(fit)
            sparse["perp"]["aggregate_trades_per_second"]=.01
            sparse["perp"]["return_std_ppm_1s"]=None
            sparse["perp"]["active_seconds"]=1
            self.assertGreater(fit_score(sparse,fit),10)

    def test_zero_direction_persistence_is_observed_data_not_missing(self):
        with tempfile.TemporaryDirectory() as directory:
            fit=reference_profile(source(directory),DAY,120)["fit"]
            fit["spot"]["taker_side_persistence"]=0
            self.assertEqual(fit_score(fit,fit),0)
            random=copy.deepcopy(fit)
            random["spot"]["taker_side_persistence"]=.5
            self.assertAlmostEqual(fit_score(random,fit),.5)

    def test_microstructure_export_and_account_ownership_are_reproducible(self):
        spec=microstructure_recipe()
        self.assertEqual(spec,json.loads((ROOT/"scripts/fixtures/microstructure_market.json").read_text(encoding="utf-8")))
        self.assertEqual(len(spec["agents"]),38)
        kinds=[a["Plugin"]["plugin_id"] for a in spec["agents"]]
        self.assertEqual(kinds.count("FundingRateTrader"),2)
        self.assertEqual(kinds.count("LeveragedTrendTrader"),3)
        remapped=microstructure_recipe("other-room",19,False)
        self.assertTrue(all(a["Plugin"]["participant"]["room_id"]=="other-room" for a in remapped["agents"]))
        self.assertFalse(remapped["autostart_agents"])

    def test_search_bounds_reject_nonfinite_and_invalid_depth(self):
        with tempfile.TemporaryDirectory() as directory:
            profile=reference_profile(source(directory),DAY,120)
            for key,value in [("activity",0),("width_scale",float("nan")),("levels",9),("levels",2.5),("depth_scale",.5)]:
                knobs=dict(KNOBS);knobs[key]=value
                with self.assertRaises(ValueError):joint_recipe(profile,knobs)

    def test_activity_and_aggressive_mix_change_finite_population_and_decisions(self):
        with tempfile.TemporaryDirectory() as directory:
            profile=reference_profile(source(directory),DAY,120)
            for activity,ratio in ((.6,.8),(1,.7)):
                spec,mapping=joint_recipe(profile,dict(KNOBS,activity=activity,market_ratio=ratio))
                self.assertTrue(mapping["initial_capital_is_finite"])
                for agent in spec["agents"]:
                    bot=agent["Plugin"]
                    if bot["plugin_id"]=="AdaptiveNoiseTrader":
                        self.assertEqual(bot["config"]["activity_ppm"],round(activity*1000000))
                        self.assertEqual(bot["config"]["market_order_ratio_ppm"],round(ratio*1000000))
                self.assertEqual(len(spec["agents"]),mapping["participants"])


if __name__=="__main__":
    unittest.main()
