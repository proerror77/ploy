import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "enrich_replay_runtime_evidence.py"


class EnrichReplayRuntimeEvidenceTests(unittest.TestCase):
    def run_script(self, payload, settlements):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            input_path = tmp_path / "replay.json"
            output_path = tmp_path / "replay-enriched.json"
            settlements_path = tmp_path / "settlements.json"
            report_path = tmp_path / "report.json"
            input_path.write_text(json.dumps(payload), encoding="utf-8")
            settlements_path.write_text(json.dumps(settlements), encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--input-json",
                    str(input_path),
                    "--output-json",
                    str(output_path),
                    "--settlements-json",
                    str(settlements_path),
                    "--report-json",
                    str(report_path),
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            return (
                json.loads(output_path.read_text(encoding="utf-8")),
                json.loads(report_path.read_text(encoding="utf-8")),
            )

    def test_enriches_matching_open_event_settlement_price(self):
        payload = {
            "runtime_evidence": {
                "events": [
                    {
                        "event_id": "evt-1",
                        "token_id": "token-up",
                        "settlement": "open",
                    }
                ]
            }
        }
        output, report = self.run_script(
            payload,
            [
                {
                    "event_id": "evt-1",
                    "token_id": "token-up",
                    "settlement": "0.00000000000000000000",
                }
            ],
        )

        event = output["runtime_evidence"]["events"][0]
        self.assertEqual(event["settlement"], "0.00000000000000000000")
        self.assertEqual(report["runtime_events_settlement_enriched"], 1)
        self.assertEqual(
            output["runtime_evidence"]["settlement_enrichment"]["settlement_price_count"],
            1,
        )

    def test_preserves_closed_or_unmatched_event_settlement(self):
        payload = {
            "runtime_evidence": {
                "events": [
                    {"event_id": "evt-1", "token_id": "token-up", "settlement": "0.5"},
                    {"event_id": "evt-2", "token_id": "token-up", "settlement": "open"},
                ]
            }
        }
        output, report = self.run_script(
            payload,
            [{"event_id": "evt-1", "token_id": "token-up", "settlement": "1"}],
        )

        self.assertEqual(output["runtime_evidence"]["events"][0]["settlement"], "0.5")
        self.assertEqual(output["runtime_evidence"]["events"][1]["settlement"], "open")
        self.assertEqual(report["runtime_events_settlement_enriched"], 0)


if __name__ == "__main__":
    unittest.main()
