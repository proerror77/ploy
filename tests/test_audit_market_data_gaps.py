import importlib.util
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "audit_market_data_gaps.py"
SPEC = importlib.util.spec_from_file_location("audit_market_data_gaps", SCRIPT)
audit = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = audit
SPEC.loader.exec_module(audit)


class AuditMarketDataGapsTests(unittest.TestCase):
    def test_freshness_gate_allows_historical_gap_but_reports_it(self):
        target = audit.GapTarget(
            "binance_price/BTCUSDT",
            "binance_price_ticks",
            "trade_time",
            600,
        )
        row = {
            "latest_at": "2026-05-09T13:05:00+08:00",
            "latest_lag_seconds": 2,
            "max_gap_minutes": 3800,
            "missing_buckets": 1120,
        }

        status, reasons, freshness_status, _, coverage_status, coverage_reasons = (
            audit.classify_gap_for_gate(row, target, "freshness")
        )

        self.assertEqual(status, "ok")
        self.assertEqual(freshness_status, "ok")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("max gap 3800m >= 15m", coverage_reasons)
        self.assertTrue(any(reason.startswith("coverage not enforced") for reason in reasons))

    def test_coverage_gate_keeps_historical_gap_critical(self):
        target = audit.GapTarget(
            "binance_lob/BTCUSDT",
            "binance_lob_ticks",
            "event_time",
            900,
        )
        row = {
            "latest_at": "2026-05-09T13:05:00+08:00",
            "latest_lag_seconds": 1,
            "max_gap_minutes": 3800,
            "missing_buckets": 1120,
        }

        status, reasons, freshness_status, _, coverage_status, _ = audit.classify_gap_for_gate(
            row, target, "coverage"
        )

        self.assertEqual(status, "critical")
        self.assertEqual(freshness_status, "ok")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("max gap 3800m >= 15m", reasons)

    def test_freshness_gate_still_blocks_stale_source(self):
        target = audit.GapTarget(
            "binance_agg_trades/BTCUSDT",
            "binance_agg_trade_ticks",
            "trade_time",
            600,
        )
        row = {
            "latest_at": "2026-05-09T12:00:00+08:00",
            "latest_lag_seconds": 1200,
            "max_gap_minutes": 0,
            "missing_buckets": 0,
        }

        status, reasons, freshness_status, _, coverage_status, _ = audit.classify_gap_for_gate(
            row, target, "freshness"
        )

        self.assertEqual(status, "critical")
        self.assertEqual(freshness_status, "critical")
        self.assertEqual(coverage_status, "ok")
        self.assertIn("latest lag 1200s > 600s", reasons)


if __name__ == "__main__":
    unittest.main()
