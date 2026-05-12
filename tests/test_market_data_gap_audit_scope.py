import importlib.util
from pathlib import Path
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
AUDIT_SCRIPT = ROOT / "scripts" / "audit_market_data_gaps.py"
WORKFLOW = ROOT / ".github" / "workflows" / "market-data-gap-audit.yml"
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


if __name__ == "__main__":
    unittest.main()
