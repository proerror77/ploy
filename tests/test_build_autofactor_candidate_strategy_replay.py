import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.test_autofactor_strategy_promotion import (
    AUTOFACTOR_REPORT,
    AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
)


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "build_autofactor_candidate_strategy_replay.py"


class BuildAutoFactorCandidateStrategyReplayTests(unittest.TestCase):
    def run_script(self, report: str, *extra_args: str) -> tuple[dict, str]:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            report_path = tmp_path / "report.txt"
            output_json = tmp_path / "candidate-strategy-replay.json"
            output_md = tmp_path / "candidate-strategy-replay.md"
            report_path.write_text(report, encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--report",
                    str(report_path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                    *extra_args,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            return (
                json.loads(output_json.read_text(encoding="utf-8")),
                output_md.read_text(encoding="utf-8"),
            )

    def test_builds_ready_replay_for_runtime_mappable_settlement_candidate(self):
        payload, markdown = self.run_script(AUTOFACTOR_SETTLEMENT_AUTO_REPORT)

        self.assertTrue(payload["promotion_ready"])
        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertEqual(payload["evidence_stage"], "executable_replay")
        self.assertEqual(payload["basis"], "factor_walk_forward_top_bucket_aggregate")
        self.assertEqual(payload["strategy_profile"], "settlement_probability")
        self.assertTrue(payload["decision_contract"]["event_level"])
        self.assertTrue(payload["decision_contract"]["one_decision_per_event"])
        self.assertTrue(payload["decision_contract"]["official_settlement"])
        self.assertTrue(payload["decision_contract"]["full_depth_entry"])
        self.assertEqual(payload["metrics"]["trade_count"], 9966)
        self.assertEqual(payload["metrics"]["unique_event_count"], 9966)
        self.assertGreater(payload["metrics"]["total_pnl"], 0)
        self.assertGreater(payload["metrics"]["roi"], 0)
        self.assertEqual(payload["blocking_risk_flags"], [])
        self.assertIn("Promotion ready: `true`", markdown)

    def test_blocks_when_only_candidate_is_wrong_profile(self):
        payload, markdown = self.run_script(
            AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "settlement_probability",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn("runtime_profile_mismatch", ",".join(payload["blocking_risk_flags"]))
        self.assertIn("Promotion ready: `false`", markdown)


if __name__ == "__main__":
    unittest.main()
