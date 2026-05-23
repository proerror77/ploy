import json
import sys
import unittest
from pathlib import Path
from unittest import mock

from scripts import run_settlement_probability_prd_gate as gate


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "settlement-probability-prd-gate.yml"


class SettlementProbabilityPrdGateTests(unittest.TestCase):
    def run_main(self, argv, *, dispatch):
        with mock.patch.object(sys, "argv", ["run_settlement_probability_prd_gate.py", *argv]):
            with mock.patch.object(
                gate,
                "refresh_run",
                return_value=gate.WorkflowRun(
                    database_id=12345,
                    status="completed",
                    conclusion="success",
                    url="https://example.invalid/run/12345",
                    created_at=gate.parse_created_at("2026-05-22T00:00:00Z"),
                ),
            ):
                with mock.patch.object(gate, "dispatch_workflow", side_effect=dispatch):
                    return gate.main()

    def test_existing_snapshot_leaves_snapshot_bound_sampling_for_manifest_resolution(self):
        dispatches = []

        def capture_dispatch(workflow, fields, *, workflow_ref, dry_run):
            dispatches.append((workflow, fields, workflow_ref, dry_run))
            return None

        status = self.run_main(
            [
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--snapshot-run-id",
                "12345",
                "--replay-parity-run-id",
                "26301157711:recorded-replay-parity-26301157711",
                "--dry-run",
            ],
            dispatch=capture_dispatch,
        )

        self.assertEqual(status, 0)
        self.assertEqual(len(dispatches), 1)
        workflow, fields, _, _ = dispatches[0]
        self.assertEqual(workflow, gate.HOSTED_WALK_FORWARD_WORKFLOW)
        options = json.loads(fields["options_json"])
        self.assertEqual(options["lob_sample_secs"], "")
        self.assertEqual(options["pm_book_sample_secs"], "")
        self.assertEqual(options["observation_sample_secs"], "")
        self.assertEqual(options["max_quote_age_secs"], "")
        self.assertEqual(options["replay_parity_run_id"], "26301157711")
        self.assertEqual(
            options["replay_parity_artifact_name"],
            "recorded-replay-parity-26301157711",
        )

    def test_blocks_missing_snapshot_by_default(self):
        dispatches = []

        def capture_dispatch(workflow, fields, *, workflow_ref, dry_run):
            dispatches.append((workflow, fields, workflow_ref, dry_run))
            return None

        status = self.run_main(
            [
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--dry-run",
            ],
            dispatch=capture_dispatch,
        )

        self.assertEqual(status, 2)
        self.assertEqual(dispatches, [])

    def test_explicit_legacy_snapshot_build_keeps_default_sampling_values(self):
        dispatches = []

        def capture_dispatch(workflow, fields, *, workflow_ref, dry_run):
            dispatches.append((workflow, fields, workflow_ref, dry_run))
            return None

        status = self.run_main(
            [
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--allow-legacy-snapshot-build",
                "--dry-run",
            ],
            dispatch=capture_dispatch,
        )

        self.assertEqual(status, 0)
        self.assertEqual(len(dispatches), 1)
        workflow, fields, _, _ = dispatches[0]
        self.assertEqual(workflow, gate.RESEARCH_SNAPSHOT_WORKFLOW)
        options = json.loads(fields["options_json"])
        self.assertEqual(options["lob_sample_secs"], 30)
        self.assertEqual(options["pm_book_sample_secs"], 30)
        self.assertEqual(options["observation_sample_secs"], 30)
        self.assertEqual(options["max_quote_age_secs"], 30)

    def test_workflow_requires_snapshot_and_does_not_expose_legacy_build(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("Required run id with a complete sampled research-snapshot artifact", workflow)
        self.assertIn("required: true", workflow)
        self.assertIn("missing-required-snapshot", workflow)
        self.assertNotIn("legacy-build", workflow)
        self.assertNotIn("--allow-legacy-snapshot-build", workflow)


if __name__ == "__main__":
    unittest.main()
