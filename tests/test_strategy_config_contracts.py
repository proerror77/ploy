from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
STRATEGY_DIR = ROOT / "config" / "strategies"


class StrategyConfigContractTests(unittest.TestCase):
    def test_pm5d_threelayer_has_one_bounded_canonical_recording_source(self) -> None:
        dryrun_configs = sorted(STRATEGY_DIR.glob("02-pm5d-threelayer.*-dryrun.toml"))
        self.assertGreaterEqual(len(dryrun_configs), 4)

        recorders = []
        for path in dryrun_configs:
            config = tomllib.loads(path.read_text())
            runtime = config.get("runtime", {})
            record_path = runtime.get("record_market_updates_to")
            if record_path:
                recorders.append((path.name, runtime))

        self.assertEqual(
            [name for name, _ in recorders],
            ["02-pm5d-threelayer.obi-soft-dryrun.toml"],
        )
        runtime = recorders[0][1]
        self.assertEqual(
            runtime["record_market_updates_to"],
            "/opt/ploy/data/recordings/pm5d-threelayer-canonical.ndjson",
        )
        self.assertGreater(runtime["record_market_updates_max_records"], 0)
        self.assertGreater(runtime["record_market_updates_max_bytes"], 0)


if __name__ == "__main__":
    unittest.main()
