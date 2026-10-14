"""Run real PostgreSQL HTTP recovery with explicit host experiments enabled."""
import argparse
import json
import os
from pathlib import Path
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "python/tests"))
from benchmark_strategy_runtime import host_experiment


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--host-experiment", type=host_experiment, action="append", default=[])
    args = parser.parse_args()
    if not os.environ.get("MARKETFORGE_TEST_DATABASE_URL"):
        parser.error("set the isolated MARKETFORGE_TEST_DATABASE_URL; never use a production database")
    os.environ.update(MARKETFORGE_TEST_SERVER=str(args.server.resolve()),
                      MARKETFORGE_REQUIRE_PINE_TESTS="1", MARKETFORGE_REQUIRE_POSTGRES_TESTS="1")
    # Freeze the public oracle before the plugin adds its private adapter path.
    import pine_compat
    if pine_compat.__version__ != "0.3.1":
        raise RuntimeError("public HTTP oracle must be pine-compat-runtime 0.3.1")
    from test_pine_inprocess_http import PineInProcessRecoveryTests
    options = dict(args.host_experiment)

    class HostRecovery(PineInProcessRecoveryTests):
        def setUp(self):
            super().setUp()
            self.env.update(options)

    result = unittest.TextTestRunner(verbosity=2).run(unittest.defaultTestLoader.loadTestsFromTestCase(HostRecovery))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps({"tests":result.testsRun,"failures":len(result.failures),
        "errors":len(result.errors),"skipped":len(result.skipped),"server":str(args.server.resolve()),
        "experiments":options,"scope":"Real authenticated HTTP, durable PostgreSQL restart and state/trade comparison. Not PostgreSQL capacity."},indent=2)+"\n")
    if not result.wasSuccessful() or result.skipped or result.testsRun != 2:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
