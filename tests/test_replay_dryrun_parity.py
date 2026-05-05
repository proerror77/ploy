import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "replay_dryrun_parity.py"


def evidence_payload(
    *,
    intent_id="intent-1",
    order_id="order-1",
    fill_id="fill-1",
    order_side="BUY",
    fill_side="BUY",
    limit_price="0.42",
    fill_price="0.41",
    deployment_id="pm5d.threelayer.test",
    created_at="2026-05-02T01:02:03Z",
    fill_timestamp="2026-05-02T01:02:04Z",
):
    return {
        "runtime_evidence": {
            "orders": [
                {
                    "deployment_id": deployment_id,
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "order_side": order_side,
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "limit_price": limit_price,
                    "filled_quantity": "10.0000000",
                    "status": "FILLED",
                    "created_at": created_at,
                }
            ],
            "fills": [
                {
                    "deployment_id": deployment_id,
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "fill_id": fill_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "fill_side": fill_side,
                    "quantity": "10",
                    "price": fill_price,
                    "fee": "0",
                    "fill_timestamp": fill_timestamp,
                }
            ],
        }
    }


class ReplayDryrunParityTests(unittest.TestCase):
    def run_script(self, replay, dryrun, extra_args=None):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            replay_path = tmp_path / "replay.json"
            dryrun_path = tmp_path / "dryrun.json"
            output_path = tmp_path / "out" / "parity.json"
            replay_path.write_text(json.dumps(replay), encoding="utf-8")
            dryrun_path.write_text(json.dumps(dryrun), encoding="utf-8")

            args = [
                sys.executable,
                str(SCRIPT),
                "--replay-json",
                str(replay_path),
                "--dryrun-json",
                str(dryrun_path),
                "--output-json",
                str(output_path),
            ]
            if extra_args:
                args.extend(extra_args)
            subprocess.run(
                args,
                check=True,
                cwd=ROOT,
                capture_output=True,
                text=True,
            )
            return json.loads(output_path.read_text(encoding="utf-8"))

    def test_runtime_evidence_parity_ready_for_matching_orders_and_fills(self):
        result = self.run_script(evidence_payload(), evidence_payload())

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "continue")
        self.assertEqual(runtime["orders"]["shared_count"], 1)
        self.assertEqual(runtime["fills"]["shared_count"], 1)
        self.assertEqual(result["blocking_risk_flags"], [])
        self.assertIn("replay_has_no_event_level_rows", result["advisory_flags"])

    def test_runtime_evidence_matches_semantic_rows_with_different_generated_ids(self):
        result = self.run_script(
            evidence_payload(order_id="replay-order", fill_id="replay-fill", order_side="UNKNOWN"),
            evidence_payload(order_id="dryrun-order", fill_id="dryrun-fill"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "continue")
        self.assertEqual(runtime["orders"]["shared_count"], 1)
        self.assertEqual(runtime["fills"]["shared_count"], 1)

    def test_runtime_evidence_blocks_price_mismatch(self):
        result = self.run_script(
            evidence_payload(fill_price="0.41"),
            evidence_payload(fill_price="0.43"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("runtime_evidence_field_mismatches", result["risk_flags"])
        self.assertIn("runtime_evidence_field_mismatches", result["blocking_risk_flags"])
        self.assertEqual(runtime["mismatches"][0]["field"], "price")

    def test_runtime_evidence_blocks_fill_side_mismatch(self):
        result = self.run_script(
            evidence_payload(fill_side="BUY"),
            evidence_payload(fill_side="SELL"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("runtime_evidence_field_mismatches", result["risk_flags"])
        self.assertEqual(runtime["mismatches"][0]["field"], "fill_side")

    def test_runtime_evidence_blocks_different_semantic_identity(self):
        result = self.run_script(
            evidence_payload(intent_id="replay-intent"),
            evidence_payload(intent_id="dryrun-intent"),
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertFalse(runtime["strict_parity_ready"])
        self.assertEqual(result["decision"], "fix-data-or-runtime-mismatch")
        self.assertIn("orders_present_in_replay_missing_from_dryrun", result["risk_flags"])
        self.assertIn("fills_present_in_replay_missing_from_dryrun", result["risk_flags"])
        self.assertEqual(runtime["orders"]["shared_count"], 0)
        self.assertEqual(runtime["fills"]["shared_count"], 0)

    def test_filters_limit_comparison_to_matching_deployment_window(self):
        replay = evidence_payload()
        dryrun = evidence_payload()
        extra = evidence_payload(
            intent_id="intent-outside-window",
            order_id="order-outside-window",
            fill_id="fill-outside-window",
            created_at="2026-05-02T01:30:03Z",
            fill_timestamp="2026-05-02T01:30:04Z",
        )
        dryrun["runtime_evidence"]["orders"].extend(extra["runtime_evidence"]["orders"])
        dryrun["runtime_evidence"]["fills"].extend(extra["runtime_evidence"]["fills"])

        result = self.run_script(
            replay,
            dryrun,
            extra_args=[
                "--deployment-id",
                "pm5d.threelayer.test",
                "--since",
                "2026-05-02T01:00:00Z",
                "--until",
                "2026-05-02T01:05:00Z",
            ],
        )

        runtime = result["runtime_evidence_comparison"]
        self.assertTrue(runtime["strict_parity_ready"])
        self.assertEqual(runtime["orders"]["dryrun_count"], 1)
        self.assertEqual(runtime["fills"]["dryrun_count"], 1)
        self.assertEqual(result["filters"]["deployment_id"], "pm5d.threelayer.test")


if __name__ == "__main__":
    unittest.main()
