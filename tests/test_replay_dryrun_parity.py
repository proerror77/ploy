import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "replay_dryrun_parity.py"


def evidence_payload(limit_price="0.42", fill_price="0.41"):
    return {
        "runtime_evidence": {
            "orders": [
                {
                    "deployment_id": "pm5d.threelayer.test",
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "order_side": "BUY",
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
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "fill_id": "fill-1",
                    "event_id": "event-1",
                    "token_id": "token-up",
                    "fill_side": "BUY",
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


if __name__ == "__main__":
    unittest.main()
