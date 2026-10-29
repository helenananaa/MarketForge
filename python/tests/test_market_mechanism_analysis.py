"""Boundary and missing-data checks for the experiment's reported statistics."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / 'scripts'))
from analyze_market_mechanisms import add_bar, asset_checks, bar_metrics, quoted_mid_metrics, rebar


class MarketAnalysisTests(unittest.TestCase):
    def test_closed_buckets_exclude_end_and_keep_ohlcv(self):
        bars={}
        for time,price,qty in [(0,100,2),(59999,105,3),(60000,99,7),(119999,102,11),(120000,200,50)]:
            add_bar(bars,time,price,qty)
        first=bars[0]
        self.assertEqual((first['open'],first['high'],first['low'],first['close'],first['volume'],first['trades']),
                         (100,105,100,105,5,2))
        metrics=bar_metrics(bars,0,120000)
        self.assertEqual(metrics['bars'],2)
        self.assertEqual(metrics['return_samples'],1)
        self.assertAlmostEqual(metrics['close_change_pct'],(102/105-1)*100)

    def test_gaps_are_not_filled_or_linked_into_returns(self):
        bars={}
        for i,price in [(0,100),(1,100),(3,120),(4,120)]:add_bar(bars,i*60000,price,1)
        metrics=bar_metrics(bars,0,300000)
        self.assertEqual(metrics['bars'],4)
        self.assertEqual(metrics['expected_bars'],5)
        self.assertEqual(metrics['return_samples'],2)
        self.assertEqual(metrics['return_std_bp'],0)
        self.assertIsNone(metrics['return_acf1'])
        self.assertEqual(metrics['zero_return_fraction'],1)

    def test_warmup_does_not_leak_into_first_evaluation_return(self):
        bars={}
        for i,price in enumerate([1,100,100,100]):add_bar(bars,i*60000,price,1)
        metrics=bar_metrics(bars,60000,240000)
        self.assertEqual(metrics['return_std_bp'],0)
        self.assertEqual(metrics['return_samples'],2)

    def test_five_minute_aggregation_preserves_quantity_and_extrema(self):
        bars={}
        for i,price in enumerate([100,103,99,101,104,102]):add_bar(bars,i*60000,price,i+1)
        merged=rebar(bars,5)
        self.assertEqual((merged[0]['open'],merged[0]['high'],merged[0]['low'],merged[0]['close']),
                         (100,104,99,104))
        self.assertEqual(sum(b['volume'] for b in merged.values()),21)
        self.assertEqual(sum(b['trades'] for b in merged.values()),6)

    def test_quote_mid_does_not_reuse_an_earlier_quote_when_minute_ends_empty(self):
        def sample(time, bid, ask):
            return dict(time_ms=time,book=dict(
                bids=[dict(price_tick=bid,qty=1)] if bid else [],
                asks=[dict(price_tick=ask,qty=1)] if ask else []))
        samples=[sample(59000,99,101),sample(118000,109,111),sample(119000,109,None),
                 sample(179000,119,121),sample(239000,119,121)]
        result=quoted_mid_metrics(samples,0,240000,1000)
        self.assertEqual(result['bars'],3)
        self.assertEqual(result['return_samples'],1)
        self.assertEqual(result['zero_return_fraction'],1)

    def test_asset_snapshot_accounting_is_checked_without_inventing_a_fee_ledger(self):
        balance=dict(asset_id='cash',total=100,available=80,portfolio_total=20,
                     portfolio_reserved=0,portfolio_available=20,venue_total=80,
                     venue_reserved=20,venue_available=60)
        snapshot=dict(accounts=[dict(account_id=1,assets=[balance])])
        data=dict(assets_before=snapshot,assets_after=snapshot)
        self.assertEqual(asset_checks(data)['total_deltas'],{'cash':0})
        balance['available']=81
        with self.assertRaises(AssertionError):asset_checks(data)


if __name__=='__main__':unittest.main()
