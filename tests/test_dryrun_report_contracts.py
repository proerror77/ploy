import importlib.util
import math
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
SUMMARY_SCRIPT = ROOT / "scripts" / "report_dryrun_summary.py"
HTML_SCRIPT = ROOT / "scripts" / "report_strategy.py"
SIDE_KEY_MIGRATION = ROOT / "migrations" / "039_fix_strategy_track_record_side_key.sql"


def load_summary_module():
    spec = importlib.util.spec_from_file_location("report_dryrun_summary", SUMMARY_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class DryRunReportContractTests(unittest.TestCase):
    def test_summary_metadata_join_uses_token_settlement_bridge(self) -> None:
        script = SUMMARY_SCRIPT.read_text()

        self.assertIn("LEFT JOIN pm_token_settlements s ON s.token_id = t.token_id", script)
        self.assertIn("LEFT JOIN pm_market_metadata m ON m.market_slug = s.market_slug", script)
        self.assertNotIn("m.market_slug = t.event_id", script)

    def test_summary_metrics_expose_explicit_sharpe_basis(self) -> None:
        summary = load_summary_module()
        events = [
            {"is_closed": True, "net_pnl": 1.0, "closed_at": "2026-04-29T01:00:00+00:00", "trade_key": "a"},
            {"is_closed": True, "net_pnl": -0.5, "closed_at": "2026-04-29T02:00:00+00:00", "trade_key": "b"},
            {"is_closed": True, "net_pnl": 1.5, "closed_at": "2026-04-29T03:00:00+00:00", "trade_key": "c"},
        ]

        metrics = summary.build_report_slice(events, [{"net_pnl": 1.0}, {"net_pnl": -0.5}])["metrics"]

        expected_trade_sharpe = (sum([1.0, -0.5, 1.5]) / 3) / math.sqrt(13 / 12) * math.sqrt(3)
        self.assertAlmostEqual(metrics["sharpe_per_trade"], expected_trade_sharpe, places=4)
        self.assertEqual(metrics["sharpe"], metrics["sharpe_per_trade"])
        self.assertEqual(metrics["sharpe_basis"], "closed_trade_pnl_sqrt_n")
        self.assertEqual(metrics["daily_sharpe_basis"], "daily_net_pnl_sqrt_365")

    def test_summary_daily_by_window_uses_cst_day(self) -> None:
        summary = load_summary_module()

        self.assertEqual(
            summary.day_from_event({"closed_at": "2026-04-28T17:30:00+00:00"}),
            "2026-04-29",
        )

    def test_execution_diagnostics_contract(self) -> None:
        summary = load_summary_module()
        diagnostics = summary.build_execution_diagnostics(
            [
                {
                    "total_orders": 3,
                    "buy_orders": 2,
                    "sell_orders": 1,
                    "rejected_orders": 1,
                    "rejected_buy_orders": 1,
                    "partial_buy_orders": 1,
                    "buy_requested_notional": 30,
                    "buy_filled_notional": 15,
                }
            ]
        )

        self.assertEqual(diagnostics["basis"], "strategy_runtime_orders")
        self.assertEqual(diagnostics["partial_buy_threshold_pct"], 98)
        self.assertEqual(diagnostics["summary"]["rejected_buy_orders"], 1)
        self.assertEqual(diagnostics["summary"]["buy_fill_rate_pct"], 50)

    def test_html_report_aggregates_orders_and_names_sharpe_bases(self) -> None:
        script = HTML_SCRIPT.read_text()

        self.assertIn("buy_requested_notional", script)
        self.assertIn("rejected_buy_orders", script)
        self.assertIn("Rejected BUY", script)
        self.assertIn("Sharpe / Trade", script)
        self.assertIn("Sharpe Daily Ann", script)
        self.assertNotIn("ORDER BY o.created_at ASC\n  LIMIT 1", script)
        self.assertNotIn("else 0.001", script)

    def test_side_key_migration_groups_by_token_and_market_side(self) -> None:
        migration = SIDE_KEY_MIGRATION.read_text()

        self.assertIn("trade_key,\n        token_id,\n        market_side", migration)
        self.assertIn("GROUP BY\n        runtime_mode,\n        strategy_id,\n        deployment_id,\n        trade_key,\n        token_id,\n        market_side", migration)


if __name__ == "__main__":
    unittest.main()
