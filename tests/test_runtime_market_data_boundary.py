from pathlib import Path
import tomllib
import unittest


ROOT = Path(__file__).resolve().parents[1]
STRATEGY_DIR = ROOT / "config" / "strategies"


class RuntimeMarketDataBoundaryTests(unittest.TestCase):
    def test_pm5d_runtime_configs_default_to_local_market_data(self) -> None:
        configs = sorted(STRATEGY_DIR.glob("02-pm5d*.toml"))
        self.assertGreaterEqual(len(configs), 1)

        for path in configs:
            config = tomllib.loads(path.read_text())
            mode = config.get("runtime", {}).get("mode", "")
            if mode not in {"dryrun", "live"}:
                continue
            source = config.get("runtime", {}).get("market_data_source", "local_db")
            self.assertEqual(
                source,
                "local_db",
                f"{path.name} must not opt strategy runners back into direct public feeds",
            )

    def test_live_runtime_direct_feeds_are_explicitly_gated(self) -> None:
        live_rs = ROOT / "crates" / "ploy-strategy-runtime" / "src" / "live.rs"
        text = live_rs.read_text()

        self.assertIn("market_data_source.uses_external_direct()", text)
        self.assertIn("market_data_source.uses_local_db()", text)
        self.assertIn("spawn_db_polymarket_feed", text)

        external_gate = text.index("market_data_source.uses_external_direct()")
        for direct_feed in (
            "spawn_spot_feed(",
            "spawn_chainlink_feed(",
            "spawn_pyth_reference_feed(",
            "spawn_market_scanner(",
            "spawn_sports_feed(",
        ):
            self.assertGreater(
                text.index(direct_feed),
                external_gate,
                f"{direct_feed} must stay behind explicit external-direct opt-in",
            )


if __name__ == "__main__":
    unittest.main()
