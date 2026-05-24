import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

from scripts.research_manager_execute_plan import (
    EXECUTE_ACK,
    build_executor_payload,
    main,
)


def plan_payload(theme: str, actions: list[str], factor_registry_summary: dict | None = None) -> dict:
    return {
        "schema_version": "research_trace_plan.v1",
        "input": {
            "market_data_health": {
                "dataset_start_ts": "2026-04-21T00:00:00Z",
                "dataset_end_ts": "2026-04-23T00:00:00Z",
            },
            "factor_registry_summary": factor_registry_summary or {},
        },
        "plan": {
            "theme": theme,
            "priority": "high",
            "evidence_stage": "factor_attribution",
            "actions": actions,
        },
    }


def base_args(**overrides):
    values = {
        "mode": "dry_run",
        "execute_ack": "",
        "git_ref": "main",
        "snapshot_run_id": "",
        "symbols": "BTCUSDT",
        "stake_usd": "15",
        "chain_remaining": 1,
        "candidate_strategy_replay_run_id": "",
        "candidate_strategy_replay_artifact_name": "",
        "full_depth_execution_surface_run_id": "",
        "full_depth_execution_surface_artifact_name": "",
        "max_snapshot_window_days": 2,
        "max_full_depth_surface_hours": 12,
        "runtime_deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
        "runtime_config_path": "/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
        "runtime_recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
        "runtime_score": "",
        "runtime_strategy_profile": "settlement_probability",
        "runtime_issue_number": "538",
        "runtime_min_trade_count": "50",
        "runtime_min_fill_rate": "0.30",
        "runtime_min_roi": "0",
        "runtime_source_target": "full_depth_settlement_executable_pnl",
        "runtime_source_horizon": "5m",
    }
    values.update(overrides)
    return Namespace(**values)


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
                max_snapshot_window_days=2,
            ),
            plan_payload("fix_data", ["rerun_snapshot_data_audit"]),
        )
        self.assertEqual("research_manager_executor.v1", payload["schema_version"])
        self.assertEqual("dry_run", payload["mode"])
        self.assertEqual(1, payload["executable_dispatch_count"])
        self.assertEqual("research-snapshot.yml", payload["dispatches"][0]["workflow"])
        self.assertEqual("2026-04-21", payload["dispatches"][0]["fields"]["start_date"])
        self.assertEqual("2026-04-23", payload["dispatches"][0]["fields"]["end_date"])
        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertTrue(options["upload_sampled_snapshot"])
        self.assertEqual("2026-04-21T00:00:00Z", options["start_ts"])
        self.assertEqual("2026-04-23T00:00:00Z", options["end_ts"])
        self.assertNotIn("upload_full_snapshot", options)

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
                max_snapshot_window_days=2,
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
                max_snapshot_window_days=2,
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

    def test_revise_prior_walk_forward_carries_typed_prior_and_evidence_artifacts(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "artifacts": [
                        {
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": "autofactor_formula:prior_candidate",
                                    "strategy_profile": "settlement_probability",
                                    "decision_contract": {
                                        "target": "tradeable_full_depth_settlement_pnl",
                                        "horizon": "5m",
                                    },
                                }
                            }
                        }
                    ]
                }
            ]
        }
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]
        payload = build_executor_payload(
            base_args(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                snapshot_run_id="26354463118",
                full_depth_execution_surface_run_id="26351573860",
                full_depth_execution_surface_artifact_name="full-depth-execution-surface-26351573860",
                candidate_strategy_replay_run_id="26355035577",
                candidate_strategy_replay_artifact_name="runtime-candidate-replay-26355035577",
            ),
            plan,
        )

        dispatch = payload["dispatches"][0]
        self.assertEqual("factor-walk-forward-v2-hosted-artifact.yml", dispatch["workflow"])
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertEqual(
            "26351573860",
            options["full_depth_execution_surface_run_id"],
        )
        self.assertEqual(
            "full-depth-execution-surface-26351573860",
            options["full_depth_execution_surface_artifact_name"],
        )
        self.assertEqual("26355035577", options["candidate_strategy_replay_run_id"])
        self.assertEqual(
            "tradeable_full_depth_settlement_pnl",
            options["alpha_search_plan_target"],
        )
        self.assertEqual("tradeable_full_depth_settlement_pnl", options["allowed_target"])
        prior = json.loads(options["alpha_search_llm_prior_json"])
        self.assertEqual("research_manager_typed_prior.v1", prior["schema_version"])
        self.assertEqual("revise_prior", prior["theme"])
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])

    def test_fix_runtime_plan_maps_to_candidate_replay_and_parity(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload(
                "fix_runtime",
                ["run_recorded_replay_parity", "compare_runtime_scorer_contract"],
            ),
        )

        self.assertEqual("dry_run", payload["mode"])
        self.assertEqual(2, payload["executable_dispatch_count"])
        workflows = [item["workflow"] for item in payload["dispatches"]]
        self.assertEqual(
            ["runtime-candidate-replay.yml", "recorded-replay-parity.yml"],
            workflows,
        )
        replay = payload["dispatches"][0]
        self.assertEqual(
            "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
            replay["fields"]["runtime_score"],
        )
        replay_options = json.loads(replay["fields"]["options_json"])
        self.assertTrue(replay_options["full_depth_entry"])
        self.assertFalse(replay_options["skip_settlement_exits"])
        self.assertEqual("full_depth_settlement_executable_pnl", replay_options["source_target"])
        self.assertEqual("5m", replay_options["source_horizon"])

    def test_fix_runtime_blocks_candidate_replay_without_runtime_score(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload("fix_runtime", ["compare_runtime_scorer_contract"]),
        )

        self.assertEqual(0, payload["executable_dispatch_count"])
        self.assertIn("missing_runtime_score", payload["blocked_dispatches"][0]["blockers"])
        self.assertIn(
            "missing_runtime_candidate_contract",
            payload["blocked_dispatches"][0]["blockers"],
        )

    def test_fix_runtime_derives_candidate_replay_score_from_plan_contract(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload(
                "candidate_to_runtime_replay",
                ["build_runtime_candidate_replay"],
                {
                    "recent_factors": [
                        {
                            "factor_name": "auto_settlement_conservative_settlement_edge",
                            "status": "candidate",
                            "target": "full_depth_settlement_executable_pnl",
                            "horizon": "5m",
                            "blockers": [],
                            "runtime_contract": {
                                "version": "autofactor_runtime_contract_v1",
                                "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                                "strategy_profile": "settlement_probability",
                                "target": "full_depth_settlement_executable_pnl",
                                "horizon": "5m",
                                "blockers": [],
                            },
                        }
                    ]
                },
            ),
        )

        self.assertEqual(1, payload["executable_dispatch_count"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("auto_settlement_conservative_settlement_edge", dispatch["selected_factor_name"])
        self.assertEqual(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            dispatch["fields"]["runtime_score"],
        )

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

    def test_typed_prior_carries_machine_readable_blocker_actions(self) -> None:
        plan = plan_payload("revise_prior", ["generate_typed_llm_prior_json"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "search_power",
                "action": "increase_distinct_event_coverage_or_reduce_selectivity",
                "reason": "Runtime replay did not produce enough distinct event trades.",
            },
            {
                "blocker_family": "execution_fillability",
                "action": "prefer_high_fillability_depth_filters",
                "reason": "Candidate selected low-fillability entries.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        prior = payload["typed_prior"]
        self.assertIsNotNone(prior)
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])
        self.assertIn(
            "prefer candidates with broader distinct-event coverage and avoid ultra-narrow buckets",
            prior["constraints"],
        )
        self.assertIn(
            "prefer candidates with stronger full-depth fillability and capacity filters",
            prior["constraints"],
        )

    def test_fix_data_plan_with_blocker_actions_generates_typed_prior(self) -> None:
        plan = plan_payload("fix_data", ["collect_full_depth_execution_surface"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "promotion_data_execution_surface",
                "action": "collect_full_depth_execution_surface",
                "reason": "Research snapshot data health reports sampled execution surface.",
            },
            {
                "blocker_family": "promotion_data_settlement",
                "action": "repair_official_settlement_coverage",
                "reason": "Official settlement labels are required for promotion.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        prior = payload["typed_prior"]
        self.assertIsNotNone(prior)
        self.assertEqual("fix_data", prior["theme"])
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])
        self.assertIn(
            "block promotion until full-depth execution-surface evidence replaces sampled snapshots",
            prior["constraints"],
        )
        self.assertIn(
            "block promotion until official settlement coverage exists for all replay-traded events",
            prior["constraints"],
        )

    def test_blocker_actions_map_to_full_depth_and_settlement_followups(self) -> None:
        plan = plan_payload("fix_data", ["rerun_snapshot_data_audit"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "promotion_data_execution_surface",
                "action": "collect_full_depth_execution_surface",
                "reason": "Research snapshot data health reports sampled execution surface.",
            },
            {
                "blocker_family": "data_settlement",
                "action": "repair_official_settlement_coverage",
                "reason": "Runtime replay traded events are missing official settlement labels.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        workflows = [item["workflow"] for item in payload["dispatches"]]
        self.assertEqual(
            [
                "research-snapshot.yml",
                "collect-full-depth-execution-surface.yml",
                "repair-official-settlement-coverage.yml",
            ],
            workflows,
        )
        self.assertTrue(payload["dispatches"][1]["ready"])
        full_depth_options = json.loads(payload["dispatches"][1]["fields"]["options_json"])
        self.assertEqual("2026-04-21T00:00:00Z", full_depth_options["start_ts"])
        self.assertEqual("2026-04-21T12:00:00Z", full_depth_options["end_ts"])
        self.assertEqual(12, full_depth_options["max_hours"])
        self.assertTrue(payload["dispatches"][2]["ready"])
        settlement_options = json.loads(payload["dispatches"][2]["fields"]["options_json"])
        self.assertEqual("dry_run", settlement_options["mode"])
        self.assertEqual("", payload["dispatches"][2]["fields"]["execute_ack"])
        self.assertEqual("2026-04-21T00:00:00Z", settlement_options["start_ts"])
        self.assertEqual("2026-04-23T00:00:00Z", settlement_options["end_ts"])

    def test_execute_mode_passes_execute_to_settlement_repair_after_ack(self) -> None:
        plan = plan_payload("fix_data", ["repair_official_settlement_coverage"])
        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK),
            plan,
        )

        self.assertEqual("execute", payload["mode"])
        self.assertEqual("repair-official-settlement-coverage.yml", payload["dispatches"][0]["workflow"])
        settlement_options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertEqual("execute", settlement_options["mode"])
        self.assertEqual(
            "repair-official-settlement-coverage",
            payload["dispatches"][0]["fields"]["execute_ack"],
        )

    def test_full_depth_action_without_snapshot_rerun_does_not_dispatch_sampled_snapshot(self) -> None:
        payload = build_executor_payload(
            base_args(),
            plan_payload("fix_data", ["collect_full_depth_execution_surface"]),
        )

        self.assertEqual(1, len(payload["dispatches"]))
        self.assertEqual(
            "collect-full-depth-execution-surface.yml",
            payload["dispatches"][0]["workflow"],
        )

    def test_cli_records_dispatch_failures_without_dropping_executor_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            out = root / "out"
            plan.write_text(
                json.dumps(
                    plan_payload(
                        "fix_runtime",
                        ["run_recorded_replay_parity", "compare_runtime_scorer_contract"],
                    )
                )
            )
            import sys

            argv = sys.argv
            attempts = [
                {"workflow": "runtime-candidate-replay.yml", "ok": True, "returncode": 0},
                {"workflow": "recorded-replay-parity.yml", "ok": False, "returncode": 1},
            ]
            try:
                sys.argv = [
                    "research_manager_execute_plan.py",
                    "--plan-json",
                    str(plan),
                    "--output-dir",
                    str(out),
                    "--mode",
                    "execute",
                    "--execute-ack",
                    EXECUTE_ACK,
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                ]
                with patch(
                    "scripts.research_manager_execute_plan._dispatch_gh_workflow",
                    side_effect=attempts,
                ):
                    main()
            finally:
                sys.argv = argv

            payload = json.loads((out / "research-manager-executor.json").read_text())
            self.assertEqual(2, len(payload["dispatch_attempts"]))
            self.assertTrue(payload["dispatch_attempts"][0]["ok"])
            self.assertFalse(payload["dispatch_attempts"][1]["ok"])
            self.assertTrue((out / "research-manager-executor.md").exists())

    def test_fix_data_snapshot_dispatch_caps_large_dataset_window(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT,ETHUSDT,SOLUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
            ),
            {
                "schema_version": "research_trace_plan.v1",
                "input": {
                    "market_data_health": {
                        "dataset_start_ts": "2026-05-16T00:00:00Z",
                        "dataset_end_ts": "2026-05-21T00:00:00Z",
                    }
                },
                "plan": {
                    "theme": "fix_data",
                    "priority": "high",
                    "evidence_stage": "factor_attribution",
                    "actions": ["rerun_snapshot_data_audit"],
                },
            },
        )

        dispatch = payload["dispatches"][0]
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertEqual("2026-05-16", dispatch["fields"]["start_date"])
        self.assertEqual("2026-05-18", dispatch["fields"]["end_date"])
        self.assertEqual("2026-05-16T00:00:00Z", options["start_ts"])
        self.assertEqual("2026-05-18T00:00:00Z", options["end_ts"])
        self.assertTrue(dispatch["bounded_window"]["truncated"])


if __name__ == "__main__":
    unittest.main()
