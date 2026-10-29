import json,sys,tempfile,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from analyze_parent_liquidity import business


class ParentAnalysisTests(unittest.TestCase):
    def fixture(self,root,modern=False,pending=None):
        name='tiny';recipe=root/'recipe.json'
        config=dict(side='Buy',target_qty=3,horizon_ms=3600000,start_after_ms=0,max_qty=2)
        if modern:config['liquidity_aware']=True
        recipe.write_text(json.dumps(dict(agents=[dict(Plugin=dict(plugin_id='ExecutionTrader',participant=dict(participant_id='p',account_id=20,instrument_id='V-BTC-SPOT'),config=config))])),encoding='utf-8')
        state=dict(execution_start_ms=7200000,completed_qty=2 if modern else 1,deadline_reached=False)
        if modern:state['execution_ledger']=dict(demand_lots=3,filled_lots=2,remaining_lots=0,expired_lots=1,pending=pending)
        prints=[dict(instrument_id='V-BTC-SPOT',time_ms=7201000,taker_account_id=20,taker_order_id=55,taker_side='Buy',qty=1,price_tick=price)for price in (100,101)]
        snapshots=[]
        for t in (7200000,7201000,7202000,7206000,7231000):
            for m in ('V-BTC-SPOT','V-BTC-PERP'):
                snapshots.append(dict(time_ms=t,instrument_id=m,phase='after_all_actions',book=dict(bids=[dict(price_tick=99,qty=2)],asks=[dict(price_tick=101,qty=3)])))
        data=dict(trade_receipts=prints,states=[dict(id='p',state=dict(Plugin=dict(data=state)))],book_snapshots=snapshots)
        (root/(name+'.receipt.json')).write_text(json.dumps(data),encoding='utf-8')
        return dict(name=name,recipe=str(recipe),seed=1,profile='liquidity-parent'if modern else'legacy',warmup_ms=7200000,steps=14400)

    def test_counts_one_executed_order_for_multiple_prints_and_exposes_legacy_progress_lag(self):
        with tempfile.TemporaryDirectory()as folder:
            root=Path(folder);result=business(root,self.fixture(root))
            self.assertEqual(result['filled_lots'],2);self.assertEqual(result['expired_lots'],1)
            self.assertEqual(result['details'][0]['saved_progress_lag_lots'],1)
            flow=next(r for r in result['flow']if r['market']=='V-BTC-SPOT')
            self.assertEqual(flow['executed_taker_orders'],1);self.assertEqual(flow['parent_multilevel_children'],1)

    def test_modern_parent_reconciles_authoritative_fills_and_rejects_missing_feedback(self):
        with tempfile.TemporaryDirectory()as folder:
            root=Path(folder);result=business(root,self.fixture(root,True))
            self.assertEqual(result['filled_lots']+result['expired_lots']+result['remaining_lots'],3)
            row=self.fixture(root,True,dict(qty=1))
            with self.assertRaisesRegex(AssertionError,'acknowledgement missing'):business(root,row)


if __name__=='__main__':unittest.main()
