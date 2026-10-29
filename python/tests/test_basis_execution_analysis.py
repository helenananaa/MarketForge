import sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from analyze_basis_execution import morphology

class BasisAnalysisTests(unittest.TestCase):
    def test_shape_statistics_keep_flat_bars_without_inventing_wicks_or_future_ranges(self):
        bars={0:dict(open=100,high=110,low=90,close=105),1:dict(open=100,high=100,low=100,close=100),
              2:dict(open=100,high=1000,low=1,close=1)}
        result=morphology(bars,0,120000)
        self.assertEqual(result['bars'],2)
        self.assertEqual(result['nonzero_range_bars'],1)
        self.assertEqual(result['median_wick_fraction'],.75)
        self.assertEqual(result['median_range_bp'],1000)
        self.assertIsNone(morphology(bars,60000,120000)['median_wick_fraction'])

if __name__=='__main__':unittest.main()
