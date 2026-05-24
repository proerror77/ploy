from pathlib import Path
from decimal import Decimal
import json
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
STRATEGY_DIR = ROOT / "config" / "strategies"
DEPLOYMENT_DIR = ROOT / "config" / "deployments"


class StrategyConfigContractTests(unittest.TestCase):
    def test_pm5d_threelayer_recording_sources_are_explicit_and_bounded(self) -> None:
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
            [
                "02-pm5d-threelayer.obi-soft-dryrun.toml",
                "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
            ],
        )
        self.assertEqual(
            [runtime["record_market_updates_to"] for _, runtime in recorders],
            [
                "/opt/ploy/data/recordings/pm5d-threelayer-canonical.ndjson",
                "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
            ],
        )
        self.assertEqual(
            len({runtime["record_market_updates_to"] for _, runtime in recorders}),
            len(recorders),
        )
        for _, runtime in recorders:
            self.assertGreater(runtime["record_market_updates_max_records"], 0)
            self.assertGreater(runtime["record_market_updates_max_bytes"], 0)

    def test_settlement_probability_dryrun_deployment_exposure_covers_strategy_sizing(self) -> None:
        strategy = tomllib.loads(
            (
                STRATEGY_DIR
                / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
            ).read_text()
        )
        deployment = json.loads(
            (
                DEPLOYMENT_DIR
                / "pm5d.threelayer.settlement-probability-btc-eth.dryrun.json"
            ).read_text()
        )

        strategy_config = strategy["strategy"]
        expected_exposure = Decimal(str(strategy_config["stake_usd"])) * Decimal(
            str(strategy_config["max_positions"])
        )
        deployment_limit = Decimal(deployment["max_gross_exposure"])

        self.assertEqual(
            deployment["bundle_id"],
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun",
        )
        self.assertGreaterEqual(deployment_limit, expected_exposure)

    def test_settlement_probability_dryrun_uses_reviewed_autofactor_candidate(self) -> None:
        strategy = tomllib.loads(
            (
                STRATEGY_DIR
                / "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
            ).read_text()
        )["strategy"]

        self.assertEqual(strategy["three_layer_strategy_profile"], "settlement_probability")
        self.assertEqual(
            strategy["three_layer_autofactor_runtime_score"],
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
        )
        self.assertEqual(Decimal(str(strategy["three_layer_min_entry_score"])), Decimal("0.10"))


if __name__ == "__main__":
    unittest.main()
