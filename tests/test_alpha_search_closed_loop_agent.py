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

    def test_reward_stagnation_generates_prior_revision(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="reward_stagnation")
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            self.assertEqual(decision["decision"], "revise_prior")
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
            self.assertEqual(decision["artifact_count"], 2)

    def test_no_selected_nodes_generates_safe_fallback_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="no_selected_nodes", selected_nodes=[])
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertTrue(decision["prior_revision_required"])
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "auto_settlement_full_depth_settlement_edge",
            )

    def test_runtime_blocker_routes_to_fix_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
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
