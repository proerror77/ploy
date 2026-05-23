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

    def test_central_market_discovery_service_owns_pm_catalog_refresh(self) -> None:
        runner_lib = ROOT / "crates" / "ploy-runner-host" / "src" / "lib.rs"
        runner_ops = ROOT / "crates" / "ploy-runner-host" / "src" / "ops.rs"
        scanner = ROOT / "crates" / "ploy-market-data" / "src" / "scanner.rs"
        service = ROOT / "deployment" / "systemd" / "ploy-market-discovery.service"
        workflow = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"

        self.assertIn("collect-markets", runner_lib.read_text())
        self.assertIn("run_market_discovery_collector", runner_ops.read_text())

        scanner_text = scanner.read_text()
        self.assertIn("MarketDiscoveryCollectorConfig", scanner_text)
        self.assertIn("refresh_crypto_catalog", scanner_text)
        self.assertIn("persist_discovered_crypto_market", scanner_text)

        service_text = service.read_text()
        self.assertIn("ExecStart=/opt/ploy/bin/ploy-runner collect-markets", service_text)
        self.assertIn("Restart=always", service_text)
        self.assertIn("MemoryMax=768M", service_text)

        workflow_text = workflow.read_text()
        self.assertIn("ploy-market-discovery.service", workflow_text)
        self.assertLess(
            workflow_text.index("systemctl restart ploy-market-discovery.service"),
            workflow_text.index("systemctl restart ploy-quote-collector.service"),
            "market discovery must refresh catalog before quote collector subscribes",
        )
        self.assertLess(
            workflow_text.index("systemctl restart ploy-market-discovery.service"),
            workflow_text.index("systemctl restart ploy-pm-trade-collector.service"),
            "market discovery must refresh catalog before trade collector polls",
        )

    def test_pm_trade_deploy_health_uses_collector_poll_not_fresh_insert(self) -> None:
        workflow = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"
        cloud_assist = ROOT / "scripts" / "ci" / "deploy_tango_cloud_assist.py"

        for path in (workflow, cloud_assist):
            text = path.read_text()
            self.assertIn("wait_for_recent_log", text)
            self.assertIn("Polymarket trade collector poll complete", text)
            self.assertIn("pm trade collector did not complete a healthy poll after deploy", text)
            self.assertIn("pm trade collector failed after deploy", text)
            if path.name == "deploy-tango-1-1.yml":
                self.assertIn('local since="\\${DEPLOY_STARTED_AT}"', text)
            else:
                self.assertIn('local since="${{DEPLOY_STARTED_AT}}"', text)
            self.assertNotIn(
                "clob_trade_ticks WHERE received_at >= NOW() - INTERVAL '5 minutes'",
                text,
                f"{path.name} must not require a fresh PM trade insert as collector health",
            )
            self.assertNotIn(
                "clob_trade_ticks is not receiving PM trade prints after deploy",
                text,
                f"{path.name} must not fail deploys when a healthy poll inserts no trades",
            )


if __name__ == "__main__":
    unittest.main()
