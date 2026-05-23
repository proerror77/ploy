import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path

from scripts.research_manager_execute_plan import (
    EXECUTE_ACK,
    build_executor_payload,
    main,
)


def plan_payload(theme: str, actions: list[str]) -> dict:
    return {
        "schema_version": "research_trace_plan.v1",
        "input": {
            "market_data_health": {
                "dataset_start_ts": "2026-04-21T00:00:00Z",
                "dataset_end_ts": "2026-04-23T00:00:00Z",
            }
        },
        "plan": {
            "theme": theme,
            "priority": "high",
            "evidence_stage": "factor_attribution",
            "actions": actions,
        },
    }


class ResearchManagerExecutePlanTest(unittest.TestCase):
    def test_fix_data_plan_creates_snapshot_dispatch_in_dry_run(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT,ETHUSDT",
                stake_usd="15",
                chain_remaining=1,
            ),
            plan_payload("fix_data", ["rerun_snapshot_data_audit"]),
        )
        self.assertEqual("research_manager_executor.v1", payload["schema_version"])
        self.assertEqual("dry_run", payload["mode"])
        self.assertEqual(1, payload["executable_dispatch_count"])
        self.assertEqual("research-snapshot.yml", payload["dispatches"][0]["workflow"])
        self.assertEqual("2026-04-21", payload["dispatches"][0]["fields"]["start_date"])

    def test_execute_mode_requires_ack(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="execute",
                execute_ack="wrong",
                git_ref="main",
                snapshot_run_id="123",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
            ),
            plan_payload("continue_search", ["continue_hosted_alpha_search"]),
        )
        self.assertEqual("dry_run", payload["mode"])
        self.assertIn("missing_execute_ack", payload["blocked_dispatches"][-1]["blockers"])

    def test_continue_search_maps_to_bounded_walk_forward(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                git_ref="main",
                snapshot_run_id="26327019766",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=2,
            ),
            plan_payload("continue_search", ["continue_hosted_alpha_search"]),
        )
        self.assertEqual("execute", payload["mode"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("factor-walk-forward-v2-hosted-artifact.yml", dispatch["workflow"])
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertTrue(options["chain_next_run"])
        self.assertEqual(1, options["chain_remaining"])

    def test_cli_writes_executor_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            out = root / "out"
            plan.write_text(json.dumps(plan_payload("revise_prior", ["generate_typed_llm_prior_json"])))
            import sys

            argv = sys.argv
            try:
                sys.argv = [
                    "research_manager_execute_plan.py",
                    "--plan-json",
                    str(plan),
                    "--output-dir",
                    str(out),
                ]
                main()
            finally:
                sys.argv = argv
            self.assertTrue((out / "research-manager-executor.json").exists())
            self.assertTrue((out / "research-manager-executor.md").exists())
            self.assertTrue((out / "next-llm-prior.json").exists())


if __name__ == "__main__":
    unittest.main()
