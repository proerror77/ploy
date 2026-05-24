import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
AUDIT_SCRIPT = ROOT / "scripts" / "audit_market_data_gaps.py"
WORKFLOW = ROOT / ".github" / "workflows" / "market-data-gap-audit.yml"
RESEARCH_SNAPSHOT_WORKFLOW = ROOT / ".github" / "workflows" / "research-snapshot.yml"
CLOUD_ASSIST_SCRIPT = ROOT / "scripts" / "ci" / "run_tango_market_data_audit_cloud_assist.py"


def load_audit_module():
    spec = importlib.util.spec_from_file_location("audit_market_data_gaps", AUDIT_SCRIPT)
    assert spec is not None
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class MarketDataGapAuditScopeTests(unittest.TestCase):
    def test_collector_profiles_exclude_research_valid_windows(self) -> None:
        audit = load_audit_module()
        collector_profiles = ("pm5d-core", "pm5d-execution", "pm5d-vol")

        for profile in collector_profiles:
            with self.subTest(profile=profile):
                requirements = audit.parse_source_requirements(profile)
                self.assertNotIn("research_valid_windows", requirements)

                research_window_target = {
                    "source_id": "research_valid_windows",
                    "table_name": "research_valid_windows",
                }
                self.assertFalse(
                    audit.source_is_required(research_window_target, requirements),
                    f"{profile} must stay scoped to market-data collectors",
                )

    def test_research_windows_remain_explicitly_auditable(self) -> None:
        audit = load_audit_module()
        requirements = audit.parse_source_requirements("research-windows")
        self.assertEqual(requirements, ["research_valid_windows"])

    def test_gap_query_uses_lookback_without_explicit_window(self) -> None:
        audit = load_audit_module()
        target = audit.GapTarget(
            "binance_price/BTCUSDT",
            "binance_price_ticks",
            "trade_time",
            600,
            filter_column="symbol",
            filter_value="BTCUSDT",
        )

        query = audit.gap_query(
            target,
            lookback_hours=168,
            bucket_minutes=5,
            recent_minutes=15,
            statement_timeout_seconds=20,
        )

        self.assertIn("now() - interval '168 hours'", query)
        self.assertIn("now() - interval '5 minutes'", query)
        self.assertNotIn("2026-05-16T00:00:00Z", query)

    def test_gap_query_uses_explicit_historical_window_when_provided(self) -> None:
        audit = load_audit_module()
        target = audit.GapTarget(
            "binance_price/BTCUSDT",
            "binance_price_ticks",
            "trade_time",
            600,
            filter_column="symbol",
            filter_value="BTCUSDT",
        )

        query = audit.gap_query(
            target,
            lookback_hours=168,
            bucket_minutes=5,
            recent_minutes=15,
            statement_timeout_seconds=20,
            start_ts="2026-05-16T00:00:00Z",
            end_ts="2026-05-21T00:00:00Z",
        )

        self.assertIn("'2026-05-16T00:00:00Z'::timestamptz", query)
        self.assertIn("'2026-05-21T00:00:00Z'::timestamptz", query)
        self.assertIn("- interval '5 minutes'", query)
        self.assertNotIn("now() - interval '168 hours'", query)

    def test_historical_window_gate_ignores_current_freshness(self) -> None:
        audit = load_audit_module()
        target = audit.GapTarget("binance_price/BTCUSDT", "binance_price_ticks", "trade_time", 600)
        row = {
            "latest_at": None,
            "latest_lag_seconds": None,
            "max_gap_minutes": 0,
            "missing_buckets": 0,
        }

        status, reasons, freshness_status, _, coverage_status, _ = audit.classify_gap_for_gate(
            row,
            target,
            "coverage",
            historical_window=True,
        )

        self.assertEqual("ok", status)
        self.assertEqual("critical", freshness_status)
        self.assertEqual("ok", coverage_status)
        self.assertTrue(
            any("freshness not enforced for historical window" in reason for reason in reasons)
        )

    def test_scheduled_workflow_uses_collector_scope(self) -> None:
        workflow = WORKFLOW.read_text()
        self.assertIn("REQUIRED_SOURCES: pm5d-vol", workflow)
        self.assertIn("--required-sources \"${REQUIRED_SOURCES}\"", workflow)
        self.assertIn("remote ${audit_kind} audit failed after ${attempt} attempts", workflow)
        self.assertIn(
            "copying ${audit_kind} audit report failed after ${attempt} attempts",
            workflow,
        )
        self.assertIn("Run remote gap audits via Cloud Assistant fallback", workflow)
        self.assertIn(str(CLOUD_ASSIST_SCRIPT.relative_to(ROOT)), workflow)
        self.assertIn(
            'if [ "${EVENT_NAME}" = "schedule" ] && [ "${EVENT_SCHEDULE}" = "17 */6 * * *" ]; then',
            workflow,
        )
        self.assertIn('gate_mode="coverage"', workflow)

    def test_research_snapshot_audit_uses_requested_dataset_window(self) -> None:
        workflow = RESEARCH_SNAPSHOT_WORKFLOW.read_text()
        self.assertIn('audit_start_ts="${SNAPSHOT_START_TS:-${{ github.event.inputs.start_date }}}"', workflow)
        self.assertIn('audit_end_ts="${SNAPSHOT_END_TS:-${{ github.event.inputs.end_date }}}"', workflow)
        self.assertIn('--start-ts "${audit_start_ts}"', workflow)
        self.assertIn('--end-ts "${audit_end_ts}"', workflow)
        self.assertIn("Gate mode: `{payload.get('gate_mode', 'unknown')}`", workflow)
        self.assertIn("Audit window: `{payload.get('audit_window_start_ts') or '<lookback-start>'}", workflow)
        self.assertIn("remote snapshot data audit failed after ${attempt} attempts", workflow)
        self.assertIn("copying snapshot data audit report failed after ${attempt} attempts", workflow)
        self.assertIn("copying research snapshot tar failed after ${attempt} attempts", workflow)


if __name__ == "__main__":
    unittest.main()
