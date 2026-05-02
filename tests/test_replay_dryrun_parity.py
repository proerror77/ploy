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
):
    return {
        "runtime_evidence": {
            "orders": [
                {
                    "deployment_id": "pm5d.threelayer.test",
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
                    "created_at": "2026-05-02T01:02:03Z",
                }
            ],
            "fills": [
                {
                    "deployment_id": "pm5d.threelayer.test",
                    "intent_id": intent_id,
                    "order_id": order_id,
                    "fill_id": fill_id,
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "fill_side": fill_side,
                    "quantity": "10",
                    "price": fill_price,
                    "fee": "0",
                    "fill_timestamp": "2026-05-02T01:02:04Z",
                }
            ],
        }
    }


class ReplayDryrunParityTests(unittest.TestCase):
    def run_script(self, replay, dryrun):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            replay_path = tmp_path / "replay.json"
            dryrun_path = tmp_path / "dryrun.json"
            output_path = tmp_path / "out" / "parity.json"
            replay_path.write_text(json.dumps(replay), encoding="utf-8")
            dryrun_path.write_text(json.dumps(dryrun), encoding="utf-8")

            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--replay-json",
                    str(replay_path),
                    "--dryrun-json",
                    str(dryrun_path),
                    "--output-json",
                    str(output_path),
                ],
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


if __name__ == "__main__":
    unittest.main()
