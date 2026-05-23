import json
import unittest
from pathlib import Path

from scripts.autofactor_accounting_catalog import (
    CATALOG_SCHEMA_VERSION,
    autofactor_target_contract,
    autofactor_target_horizon,
    validate_autofactor_source_contract,
)


ROOT = Path(__file__).resolve().parents[1]
CATALOG = ROOT / "config" / "autofactor_accounting_catalog.json"


class AutoFactorAccountingCatalogTest(unittest.TestCase):
    def test_catalog_schema_and_required_targets(self) -> None:
        payload = json.loads(CATALOG.read_text(encoding="utf-8"))
        self.assertEqual(payload["schema_version"], CATALOG_SCHEMA_VERSION)
        targets = payload["targets"]
        for target, horizon, lane in [
            ("reprice_pnl_5s", "5s", "repricing"),
            ("full_depth_reprice_pnl_30s", "30s", "repricing"),
            ("full_depth_settlement_executable_pnl", "5m", "settlement_probability"),
            ("tradeable_full_depth_settlement_pnl", "5m", "settlement_probability"),
        ]:
            with self.subTest(target=target):
                contract = targets[target]
                self.assertEqual(contract["horizon"], horizon)
                self.assertEqual(contract["accounting_lane"], lane)
                self.assertEqual(autofactor_target_horizon(target), horizon)
                self.assertEqual(autofactor_target_contract(target), contract)

    def test_source_contract_validation_blocks_unknown_or_mismatched_horizon(self) -> None:
        self.assertEqual(
            validate_autofactor_source_contract(
                target="full_depth_settlement_executable_pnl",
                horizon="5m",
            ),
            [],
        )
        self.assertEqual(
            validate_autofactor_source_contract(
                target="full_depth_settlement_executable_pnl",
                horizon="30s",
            ),
            ["source_horizon_mismatch:30s!=5m"],
        )
        self.assertEqual(
            validate_autofactor_source_contract(target="experimental", horizon="5m"),
            ["unknown_source_target:experimental"],
        )


if __name__ == "__main__":
    unittest.main()
