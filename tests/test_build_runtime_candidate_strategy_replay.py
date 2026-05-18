import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "build_runtime_candidate_strategy_replay.py"


def runtime_eval_payload(*, settled=True, pnl="12.5"):
    settlement = "1" if settled else "open"
    return {
        "artifact_type": "strategy_runtime_evaluation",
        "result": {
            "updates_processed": 1000,
            "intents_submitted": 2,
            "fills_recorded": 2,
            "strategy_diagnostics": {"candidate_events": 2},
        },
        "runtime_evidence": {
            "basis": "trading_runtime_snapshot",
            "intents": [
                {
                    "intent_id": "intent-1",
                    "event_id": "event-1",
                    "quantity": "10",
                    "limit_price": "0.42",
                },
                {
                    "intent_id": "intent-2",
                    "event_id": "event-2",
                    "quantity": "10",
                    "limit_price": "0.43",
                },
            ],
            "orders": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "filled_quantity": "10",
                    "status": "FILLED",
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "filled_quantity": "10",
                    "status": "FILLED",
                },
            ],
            "fills": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "price": "0.42",
                    "fee": "0",
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "price": "0.43",
                    "fee": "0",
                },
            ],
            "events": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "signal_inputs": {"purpose": "ENTRY"},
                    "settlement": settlement,
                    "pnl": pnl,
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "signal_inputs": {"purpose": "ENTRY"},
                    "settlement": settlement,
                    "pnl": "0",
                },
            ],
        },
    }


class BuildRuntimeCandidateStrategyReplayTests(unittest.TestCase):
    def run_script(self, payload, *extra_args):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            runtime_json = tmp_path / "runtime-eval.json"
            output_json = tmp_path / "candidate-strategy-replay.json"
            output_md = tmp_path / "candidate-strategy-replay.md"
            runtime_json.write_text(json.dumps(payload), encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--runtime-evaluation-json",
                    str(runtime_json),
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
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

    def test_builds_ready_runtime_market_update_replay_artifact(self):
        payload, markdown = self.run_script(
            runtime_eval_payload(),
            "--full-depth-entry",
            "--min-trade-count",
            "2",
        )

        self.assertTrue(payload["promotion_ready"])
        self.assertEqual(payload["basis"], "runtime_market_update_replay")
        self.assertEqual(payload["metrics"]["trade_count"], 2)
        self.assertEqual(payload["metrics"]["unique_event_count"], 2)
        self.assertEqual(payload["metrics"]["settlement_event_count"], 2)
        self.assertEqual(payload["metrics"]["entry_fill_rate"], 1.0)
        self.assertGreater(payload["metrics"]["roi"], 0)
        self.assertEqual(payload["blocking_risk_flags"], [])
        self.assertIn("Promotion ready: `true`", markdown)

    def test_blocks_zero_intent_runtime_replay_with_diagnostics(self):
        payload, _ = self.run_script(
            {
                "result": {
                    "updates_processed": 172368,
                    "intents_submitted": 0,
                    "fills_recorded": 0,
                    "strategy_diagnostics": {
                        "candidate_events": 260,
                        "skip_edge_score": 183,
                    },
                },
                "runtime_evidence": {
                    "basis": "trading_runtime_snapshot",
                    "events": [],
                    "orders": [],
                    "fills": [],
                    "intents": [],
                },
            },
            "--full-depth-entry",
            "--min-trade-count",
            "2",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["metrics"]["updates_processed"], 172368)
        self.assertEqual(payload["metrics"]["trade_count"], 0)
        self.assertIn("trade_count_too_small:0<2", payload["blocking_risk_flags"])
        self.assertIn("zero_runtime_orders_and_fills", payload["blocking_risk_flags"])
        self.assertEqual(payload["strategy_diagnostics"]["skip_edge_score"], 183)

    def test_blocks_open_settlement_and_unconfirmed_depth(self):
        payload, _ = self.run_script(
            runtime_eval_payload(settled=False),
            "--min-trade-count",
            "2",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertIn("official_settlement_missing:0<2", payload["blocking_risk_flags"])
        self.assertIn("full_depth_entry_not_confirmed", payload["blocking_risk_flags"])

    def test_resolves_missing_order_event_ids_and_ignores_settlement_exits(self):
        payload = runtime_eval_payload()
        evidence = payload["runtime_evidence"]
        evidence["intents"] = []
        evidence["orders"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-down-101",
            },
        ]
        evidence["fills"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": None,
                "market_id": None,
                "fill_side": "BUY",
                "purpose": None,
                "quantity": "10",
                "price": "0.42",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": None,
                "market_id": None,
                "fill_side": "SELL",
                "purpose": None,
                "quantity": "10",
                "price": "1",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": None,
                "market_id": None,
                "fill_side": "BUY",
                "purpose": None,
                "quantity": "10",
                "price": "0.43",
                "token_id": "token-down-101",
            },
        ]
        evidence["events"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": "100",
                "market_id": "100",
                "side": "BUY",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "1",
                "pnl": "5",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": "100",
                "market_id": "100",
                "side": "SELL",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "open",
                "pnl": "10",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": "101",
                "market_id": "101",
                "side": "BUY",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "1",
                "pnl": "7",
                "token_id": "token-down-101",
            },
        ]

        artifact, _ = self.run_script(payload, "--full-depth-entry", "--min-trade-count", "2")

        self.assertTrue(artifact["promotion_ready"])
        self.assertEqual(artifact["metrics"]["trade_count"], 2)
        self.assertEqual(artifact["metrics"]["unique_event_count"], 2)
        self.assertEqual(artifact["metrics"]["settlement_event_count"], 2)
        self.assertEqual(artifact["metrics"]["total_pnl"], 12.0)
        self.assertEqual(artifact["metrics"]["entry_fill_rate"], 1.0)


if __name__ == "__main__":
    unittest.main()
