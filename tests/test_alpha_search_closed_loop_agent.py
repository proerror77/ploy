import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts import alpha_search_closed_loop_agent as agent

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "alpha_search_closed_loop_agent.py"


def write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


def artifact(
    root: Path,
    *,
    handoff_status: str = "blocked",
    chain_reason: str = "chain_next_run_false",
    should_dispatch: bool = False,
    selected_nodes: list[dict] | None = None,
    feedback: dict | None = None,
    promotion: dict | None = None,
    candidate_strategy_replay: dict | None = None,
    write_feedback: bool = True,
) -> Path:
    target = "full_depth_settlement_executable_pnl"
    factor_root = root / "factor-walk-forward-v2"
    alpha_root = factor_root / "alpha-search" / target
    if write_feedback:
        write_json(
            alpha_root / "search-feedback.json",
            feedback
            or {
                "target": target,
                "candidate_count": 4,
                "rejected_count": 1,
                "passed_count": 0,
                "best_candidate": "auto_settlement_full_depth_settlement_edge",
                "best_reward": 1.25,
            },
        )
    write_json(
        alpha_root / "mcts-expansion-plan.json",
        {
            "target": target,
            "selected_nodes": selected_nodes
            if selected_nodes is not None
            else [
                {
                    "factor_name": "auto_settlement_full_depth_settlement_edge",
                    "selected_dimension": "execution_quality",
                    "proposed_mutation": "add_capacity_gate",
                }
            ],
        },
    )
    write_json(
        alpha_root / "search-space.json",
        {
            "target": target,
            "feature_pool": [
                "entry_capacity_score",
                "full_depth_entry_fillable_gate",
                "near_strike_score",
                "side_spread",
            ],
        },
    )
    write_json(
        factor_root / "autofactor-strategy-handoff.json",
        {
            "status": handoff_status,
            "recommended_action": "create_dry_run_handoff"
            if handoff_status == "ready"
            else "do_not_promote",
        },
    )
    write_json(
        factor_root / "autofactor-strategy-promotion.json",
        promotion or {"decision": "blocked", "evaluated_factors": []},
    )
    write_json(
        root / "alpha-search-chain" / "chain-decision.json",
        {
            "current_run_id": "123456789",
            "reason": chain_reason,
            "should_dispatch": should_dispatch,
        },
    )
    if candidate_strategy_replay is not None:
        write_json(
            root / "candidate-strategy-replay" / "candidate-strategy-replay.json",
            candidate_strategy_replay,
        )
    return root


def run_artifact(parent: Path, run_id: str, *, best_reward: float) -> Path:
    return artifact(
        parent / f"factor-walk-forward-v2-{run_id}",
        feedback={
            "target": agent.DEFAULT_TARGET,
            "candidate_count": 4,
            "rejected_count": 1,
            "passed_count": 0,
            "best_candidate": "auto_settlement_full_depth_settlement_edge",
            "best_reward": best_reward,
        },
    )


class AlphaSearchClosedLoopAgentTest(unittest.TestCase):
    def test_ready_handoff_wins(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), handoff_status="ready")
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "ready_handoff")
            self.assertFalse(decision["allow_dispatch"])
            self.assertFalse(decision["profit_claim"])
            self.assertFalse(decision["external_llm_called"])
            self.assertFalse(decision["prior_revision_required"])

    def test_continue_search_when_chain_dispatches(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="continue", should_dispatch=True)
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "continue_search")
            self.assertTrue(decision["allow_dispatch"])

    def test_reward_stagnation_generates_prior_revision(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="reward_stagnation")
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])

    def test_high_rejection_generates_prior_from_selected_nodes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 10,
                    "rejected_count": 8,
                    "passed_count": 0,
                    "best_candidate": "auto_settlement_full_depth_settlement_edge",
                    "best_reward": 1.25,
                },
            )
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_capacity_gate")
            self.assertEqual(prior["mutations"][0]["feature"], "full_depth_entry_fillable_gate")

    def test_multiple_artifacts_detect_reward_stagnation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = run_artifact(root, "11111111", best_reward=2.0)
            second = run_artifact(root, "22222222", best_reward=2.0)
            decision = agent.closed_loop_decision(
                [
                    agent.load_artifact(first, agent.DEFAULT_TARGET),
                    agent.load_artifact(second, agent.DEFAULT_TARGET),
                ]
            )
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertEqual(decision["artifact_count"], 2)

    def test_no_selected_nodes_generates_safe_fallback_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="no_selected_nodes", selected_nodes=[])
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "auto_settlement_model_full_depth_settlement_edge",
            )

    def test_runtime_blocker_routes_to_fix_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {"blockers": ["missing_runtime_strategy_mapping"]}
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")

    def test_execution_blockers_take_priority_over_runtime_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "one_event_decision_violation:max_event_decisions=3",
                                "missing_runtime_strategy_mapping",
                            ]
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])

    def test_non_target_factor_blockers_do_not_drive_target_action(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "target_not_allowed",
                                "one_event_decision_violation:max_event_decisions=4",
                            ],
                            "factor": {"target": "full_depth_reprice_pnl_10s"},
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "continue_search")

    def test_missing_recorded_replay_artifact_routes_to_fix_workflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ]
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")
            self.assertFalse(decision["allow_dispatch"])

    def test_target_factor_blockers_take_priority_over_handoff_global_blockers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "global_promotion_gate_not_ready:recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ],
                            "factor": {"target": agent.DEFAULT_TARGET},
                        }
                    ],
                },
            )
            handoff = path / "factor-walk-forward-v2" / "autofactor-strategy-handoff.json"
            payload = json.loads(handoff.read_text(encoding="utf-8"))
            payload["promotion_gate"] = {
                "blocked_gates": [
                    "global_full_depth_entry_fillability: global_full_depth_entry_fill_rate=0.1311 min_required=0.3000"
                ]
            }
            write_json(handoff, payload)

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")

    def test_runtime_mappable_target_candidate_blockers_take_priority(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "top_bucket_entry_sweep_slippage_too_high:450.00>200.00",
                                "missing_runtime_strategy_mapping",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": None,
                        },
                        {
                            "blockers": [
                                "global_promotion_gate_not_ready:recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "autofactor_formula:edge",
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")

    def test_profile_matched_runtime_candidate_blockers_take_priority(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "runtime_profile_mismatch:spread_adjusted_external_move:repricing_momentum!=settlement_probability"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "repricing_momentum",
                                "strategy_profile": "repricing_momentum",
                            },
                        },
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                decision["promotion_blockers"],
                [
                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                ],
            )

    def test_runtime_replay_candidate_blockers_take_priority_within_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            other_runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_capacity"
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "top_bucket_entry_sweep_slippage_too_high:450.00>200.00",
                                f"candidate_strategy_replay_runtime_score_mismatch:{runtime_score}!={other_runtime_score}",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": other_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                decision["promotion_blockers"],
                [
                    "requires_runtime_replay_not_top_bucket_aggregate",
                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                ],
            )

    def test_fix_runtime_includes_runtime_replay_request(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            request = decision["runtime_replay_request"]
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(request["workflow"], "runtime-candidate-replay.yml")
            self.assertEqual(request["git_ref"], "main")
            self.assertEqual(request["inputs"]["runtime_score"], runtime_score)
            self.assertEqual(request["inputs"]["strategy_profile"], "settlement_probability")

    def test_fix_runtime_request_uses_best_current_runtime_mappable_factor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stale_runtime_score = (
                "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            )
            better_runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_spread_adjusted"
            )
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "name": "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.10,
                            },
                            "runtime_mapping": {
                                "runtime_score": stale_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                f"candidate_strategy_replay_runtime_score_mismatch:{stale_runtime_score}!={better_runtime_score}",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.22,
                            },
                            "runtime_mapping": {
                                "runtime_score": better_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": stale_runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )

            request = decision["runtime_replay_request"]
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                request["source_factor"],
                "mut_spread_adjusted_external_move_spread_adjusted",
            )
            self.assertEqual(request["inputs"]["runtime_score"], better_runtime_score)

    def test_runtime_pass_through_collapse_generates_targeted_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_spread_adjusted_external_move_squashed"
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                candidate_strategy_replay={
                    "basis": "runtime_market_update_replay",
                    "promotion_ready": False,
                    "runtime_score": runtime_score,
                    "metrics": {
                        "trade_count": 2,
                        "unique_event_count": 2,
                        "roi": -0.22916199066660334,
                    },
                    "score_counterfactual": {
                        "configured_entry_threshold": "0.25",
                        "depth_fillable": 2918,
                        "direct_pass_counts": {
                            "0.05": 1188,
                            "0.10": 1059,
                            "0.15": 936,
                            "0.25": 744,
                        },
                        "formula_evaluations": 2918,
                    },
                    "strategy_diagnostics": {
                        "entry_signals": 2,
                        "settlement_autofactor_depth_fillable": 2918,
                        "settlement_autofactor_executable_edge_pass_min_edge": 5,
                        "settlement_autofactor_formula_evaluations": 2918,
                        "skip_edge_score": 742,
                        "skip_entry_score": 2174,
                        "skip_settlement_side_score": 2017,
                    },
                },
                promotion={
                    "decision": "blocked",
                    "candidate_strategy_replay": {
                        "basis": "runtime_market_update_replay",
                        "ready": False,
                        "runtime_score": runtime_score,
                        "metrics": {"trade_count": 2, "roi": -0.22916199066660334},
                        "blockers": [
                            "trade_count_too_small:2<50",
                            "roi_too_low:-0.229162<0.000000",
                        ],
                    },
                    "evaluated_factors": [],
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(decision["reason"], "runtime_pass_through_collapse")
            self.assertIn(
                "runtime_entry_pass_through_too_low:2/744<50",
                decision["promotion_blockers"],
            )
            self.assertEqual(
                decision["runtime_pass_through_feedback"]["metrics"][
                    "executable_edge_pass_min_edge"
                ],
                5,
            )
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "mut_spread_adjusted_external_move_squashed",
            )
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_spread_penalty")
            self.assertEqual(prior["mutations"][0]["feature"], "side_spread")
            self.assertEqual(prior["mutations"][1]["mutation_type"], "add_capacity_gate")

    def test_cli_markdown_includes_runtime_replay_request(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            path = artifact(
                root,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )
            output_json = root / "decision.json"
            output_md = root / "decision.md"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    str(path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            decision = json.loads(output_json.read_text(encoding="utf-8"))
            markdown = output_md.read_text(encoding="utf-8")
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertIn("## Runtime Replay Request", markdown)
            self.assertIn(f"- runtime_score: `{runtime_score}`", markdown)

    def test_missing_feedback_routes_to_fix_data(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), write_feedback=False)
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_data")

    def test_cli_writes_json_markdown_and_prior_for_revise_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path = artifact(
                root,
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 10,
                    "rejected_count": 8,
                    "passed_count": 0,
                    "best_candidate": "auto_settlement_full_depth_settlement_edge",
                    "best_reward": 1.25,
                },
            )
            output_json = root / "decision.json"
            output_md = root / "decision.md"
            output_prior = root / "llm-priors-draft.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    str(path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                    "--output-prior-json",
                    str(output_prior),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            decision = json.loads(output_json.read_text(encoding="utf-8"))
            prior = json.loads(output_prior.read_text(encoding="utf-8"))
            markdown = output_md.read_text(encoding="utf-8")
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(prior["kind"], "typed_llm_prior_draft")
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_capacity_gate")
            self.assertIn("No profitability guarantee", markdown)


if __name__ == "__main__":
    unittest.main()
