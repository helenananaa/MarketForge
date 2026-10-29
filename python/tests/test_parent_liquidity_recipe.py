import copy,json,sys,unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).resolve().parents[2]/'scripts'))
from research_parent_liquidity import generated,recipe_for,PREVIOUS
from strategy_ecosystem import population


class ParentRecipeTests(unittest.TestCase):
    def test_generation_changes_only_execution_version_and_opt_in_flag(self):
        for seed in (41,7,19,73,271):
            control=generated(seed);snapshot=copy.deepcopy(control)
            candidate=generated(seed,'touch-budget')
            self.assertEqual(candidate,recipe_for(control));self.assertEqual(control,snapshot)
            if seed!=271:
                old=json.loads((PREVIOUS/'main'/f'native-s{seed}-raw-flow.recipe.json').read_text())
                self.assertEqual(control,old)
            self.assertEqual(candidate['scenario'],control['scenario'])

    def test_invalid_policy_or_unsupported_ecology_is_rejected(self):
        for mode in ('unknown','touch-budget'):
            with self.assertRaises(ValueError):population(parent_execution=mode)
        spec,_=population()
        self.assertTrue(all('liquidity_aware'not in a['Plugin']['config']for a in spec['agents']))


if __name__=='__main__':unittest.main()
