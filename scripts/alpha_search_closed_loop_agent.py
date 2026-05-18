#!/usr/bin/env python3
"""Classify alpha-search artifacts into the next closed-loop research action.

This script does not call an LLM and does not promote strategies. It turns the
existing CI artifact bundle into an auditable next action: continue the MCTS
chain, revise the typed prior, fix data/workflow/runtime gates, or hand off a
ready strategy through the existing promotion path.
"""

from __future__ import annotations

import argparse
import json
import math
import re
from pathlib import Path
from typing import Any


DEFAULT_TARGET = "full_depth_settlement_executable_pnl"
RUN_ID_RE = re.compile(r"(\d{8,})")

ALLOWED_MUTATIONS = {
    "add_feature_gate",
    "replace_denominator",
    "add_spread_penalty",
    "add_capacity_gate",
    "add_near_strike_interaction",
    "change_time_window",
    "clip_or_squash",
    "invert_or_contrarian",
    "remove_component",
}

DIMENSION_TO_MUTATION = {
    "sample_power": "add_feature_gate",
    "stability": "add_feature_gate",
    "effectiveness": "replace_denominator",
    "monotonicity": "clip_or_squash",
    "execution_quality": "add_capacity_gate",
    "overfit_risk": "remove_component",
    "exploit": "add_capacity_gate",
}

PREFERRED_FEATURES = {
    "add_feature_gate": ["near_strike_score", "quote_freshness_score", "pm_lag_score"],
    "add_capacity_gate": ["full_depth_entry_fillable_gate", "entry_capacity_score"],
    "add_near_strike_interaction": ["near_strike_score"],
    "add_spread_penalty": ["side_spread"],
    "replace_denominator": ["side_spread", "entry_capacity_score"],
}


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def optional_json(path: Path) -> Any:
    if not path.exists():
        return None
    return load_json(path)


def artifact_root(path: Path) -> Path:
    if (path / "factor-walk-forward-v2").is_dir():
        return path / "factor-walk-forward-v2"
    return path


def infer_run_id(path: Path, chain: dict[str, Any]) -> str:
    value = chain.get("current_run_id")
    if value:
        return str(value)
    for part in reversed(path.parts):
        if match := RUN_ID_RE.search(part):
            return match.group(1)
    return ""


def load_artifact(path: Path, target: str) -> dict[str, Any]:
    root = artifact_root(path)
    alpha_root = root / "alpha-search" / target
    chain = optional_json(path / "alpha-search-chain" / "chain-decision.json")
    if chain is None:
        chain = optional_json(root.parent / "alpha-search-chain" / "chain-decision.json")
    chain = chain or {}
    return {
        "artifact_dir": str(path),
        "root": root,
        "run_id": infer_run_id(path, chain),
        "target": target,
        "feedback": optional_json(alpha_root / "search-feedback.json") or {},
        "plan": optional_json(alpha_root / "mcts-expansion-plan.json") or {},
        "state": optional_json(alpha_root / "mcts-state.json") or {},
        "handoff": optional_json(root / "autofactor-strategy-handoff.json") or {},
        "promotion": optional_json(root / "autofactor-strategy-promotion.json") or {},
        "chain": chain,
        "search_space": optional_json(alpha_root / "search-space.json") or {},
    }


def as_float(value: Any) -> float | None:
    if value is None:
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(number):
        return None
    return number


def selected_nodes(plan: dict[str, Any]) -> list[dict[str, Any]]:
    value = plan.get("selected_nodes")
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, dict)]


def blocker_strings(payload: Any) -> list[str]:
    out: list[str] = []
    if isinstance(payload, dict):
        for key, value in payload.items():
            if key == "blockers" and isinstance(value, list):
                out.extend(str(item) for item in value)
            elif key in {"blocking_risk_flags", "blocked_gates"} and isinstance(value, list):
                out.extend(str(item) for item in value)
            else:
                out.extend(blocker_strings(value))
    elif isinstance(payload, list):
        for item in payload:
            out.extend(blocker_strings(item))
    return out


def target_blocker_strings(payload: Any, target: str) -> list[str]:
    out: list[str] = []
    if isinstance(payload, dict):
        evaluated = payload.get("evaluated_factors")
        if isinstance(evaluated, list):
            for item in evaluated:
                if not isinstance(item, dict):
                    continue
                factor = item.get("factor")
                factor_target = factor.get("target") if isinstance(factor, dict) else None
                if factor_target in {None, target}:
                    blockers = item.get("blockers", [])
                    if isinstance(blockers, list):
                        out.extend(str(blocker) for blocker in blockers)
                    else:
                        out.extend(blocker_strings(blockers))
            payload = {key: value for key, value in payload.items() if key != "evaluated_factors"}
        out.extend(blocker_strings(payload))
    else:
        out.extend(blocker_strings(payload))
    return out


def classify_blockers(blockers: list[str]) -> str | None:
    text = " ".join(blockers).lower()
    if not text:
        return None
    if any(
        token in text
        for token in [
            "one_event_decision_violation",
            "top_bucket_entry_sweep",
            "entry_sweep_slippage",
            "fill_rate",
            "fillability",
            "capacity",
            "depth",
        ]
    ):
        return "revise_prior"
    if any(
        token in text
        for token in [
            "recorded_replay_parity: blocked: no recorded replay parity artifact",
            "no recorded replay parity artifact",
            "missing-artifact",
            "missing_artifact",
        ]
    ):
        return "fix_workflow"
    if any(token in text for token in ["parity", "runtime", "mapping", "profile"]):
        return "fix_runtime"
    if any(
        token in text
        for token in ["settlement", "official", "data", "coverage", "missing", "freshness"]
    ):
        return "fix_data"
    return None


def latest_run(runs: list[dict[str, Any]]) -> dict[str, Any]:
    return runs[-1]


def best_run(runs: list[dict[str, Any]]) -> dict[str, Any] | None:
    scored = [
        run
        for run in runs
        if as_float(run["feedback"].get("best_reward")) is not None
    ]
    if not scored:
        return None
    return max(scored, key=lambda run: as_float(run["feedback"].get("best_reward")) or float("-inf"))


def closed_loop_decision(runs: list[dict[str, Any]]) -> dict[str, Any]:
    current = latest_run(runs)
    handoff = current["handoff"]
    chain = current["chain"]
    feedback = current["feedback"]
    plan = current["plan"]
    target_blockers = target_blocker_strings(current["promotion"], current["target"])
    handoff_blockers = blocker_strings(handoff)
    blockers = target_blockers or handoff_blockers
    blocker_action = classify_blockers(blockers)

    candidate_count = int(feedback.get("candidate_count") or 0)
    rejected_count = int(feedback.get("rejected_count") or 0)
    reject_ratio = rejected_count / candidate_count if candidate_count > 0 else 0.0
    current_reward = as_float(feedback.get("best_reward"))
    prior_rewards = [
        as_float(run["feedback"].get("best_reward"))
        for run in runs[:-1]
        if as_float(run["feedback"].get("best_reward")) is not None
    ]
    prior_best_reward = max(prior_rewards) if prior_rewards else None

    if handoff.get("status") == "ready":
        action = "ready_handoff"
        reason = "autofactor_strategy_handoff_ready"
    elif blocker_action in {"fix_runtime", "fix_data", "fix_workflow", "revise_prior"}:
        action = blocker_action
        reason = f"promotion_blockers_require_{blocker_action}"
    elif not feedback:
        action = "fix_data"
        reason = "missing_search_feedback"
    elif candidate_count == 0:
        action = "fix_data"
        reason = "zero_search_candidates"
    elif chain.get("reason") == "reward_stagnation":
        action = "revise_prior"
        reason = "reward_stagnation"
    elif (
        current_reward is not None
        and prior_best_reward is not None
        and current_reward <= prior_best_reward
    ):
        action = "revise_prior"
        reason = "latest_best_reward_did_not_improve"
    elif chain.get("reason") == "no_selected_nodes":
        action = "revise_prior"
        reason = "no_selected_nodes"
    elif not selected_nodes(plan):
        action = "revise_prior"
        reason = "mcts_plan_has_no_selected_nodes"
    elif reject_ratio >= 0.75:
        action = "revise_prior"
        reason = "high_rejected_candidate_ratio"
    elif chain.get("reason") == "continue" and chain.get("should_dispatch") is True:
        action = "continue_search"
        reason = "mcts_chain_requested_next_run"
    elif chain.get("reason") in {
        "missing_mcts_expansion_plan",
        "missing_current_search_feedback",
        "missing_best_reward",
        "invalid_best_reward",
    }:
        action = "fix_data"
        reason = str(chain.get("reason"))
    elif handoff.get("status") == "blocked":
        action = blocker_action or "revise_prior"
        reason = "handoff_blocked_without_ready_strategy"
    else:
        action = "continue_search"
        reason = "search_has_expandable_mcts_nodes"

    allow_dispatch = (
        action == "continue_search"
        and chain.get("reason") == "continue"
        and chain.get("should_dispatch") is True
    )
    best = best_run(runs)
    return {
        "schema_version": 1,
        "kind": "alpha_search_closed_loop_decision",
        "target": current["target"],
        "decision": action,
        "action": action,
        "allow_dispatch": allow_dispatch,
        "reason": reason,
        "guarantee": "No profitability guarantee. This artifact only selects the next evidence-producing action.",
        "profit_claim": False,
        "external_llm_called": False,
        "evidence_stage": "walk_forward",
        "current_run": summarize_run(current),
        "best_run": summarize_run(best) if best else None,
        "artifact_count": len(runs),
        "runs": [summarize_run(run) for run in runs],
        "promotion_blockers": blockers,
        "next_steps": next_steps(action),
        "prior_revision_required": action == "revise_prior",
    }


def summarize_run(run: dict[str, Any]) -> dict[str, Any]:
    feedback = run["feedback"]
    handoff = run["handoff"]
    chain = run["chain"]
    plan_nodes = selected_nodes(run["plan"])
    return {
        "artifact_dir": run["artifact_dir"],
        "run_id": run["run_id"],
        "candidate_count": feedback.get("candidate_count"),
        "passed_count": feedback.get("passed_count"),
        "rejected_count": feedback.get("rejected_count"),
        "best_candidate": feedback.get("best_candidate"),
        "best_reward": feedback.get("best_reward"),
        "selected_node_count": len(plan_nodes),
        "top_selected_factor": plan_nodes[0].get("factor_name") if plan_nodes else None,
        "handoff_status": handoff.get("status"),
        "recommended_action": handoff.get("recommended_action"),
        "chain_reason": chain.get("reason"),
        "chain_should_dispatch": chain.get("should_dispatch"),
    }


def next_steps(action: str) -> list[str]:
    if action == "ready_handoff":
        return [
            "Run the ready-only AutoFactor promotion/config PR path.",
            "Do not deploy live; dry-run promotion still requires normal PR and runtime gates.",
        ]
    if action == "continue_search":
        return [
            "Dispatch the next bounded hosted alpha-search iteration with the current MCTS plan.",
            "Keep evidence stage as walk_forward until promotion gates pass.",
        ]
    if action == "revise_prior":
        return [
            "Review weak dimensions and generated typed prior mutations.",
            "Pass the generated prior JSON through options_json.alpha_search_llm_prior_json on the next search run.",
        ]
    if action == "fix_data":
        return [
            "Fix missing or weak data surfaces before additional model/search tuning.",
            "Rebuild or choose a snapshot with official settlement, full-depth CLOB, and adequate event coverage.",
        ]
    if action == "fix_runtime":
        return [
            "Fix runtime scorer/config/parity mapping before promoting any factor.",
            "Rerun recorded replay/dry-run parity after the runtime fix.",
        ]
    return [
        "Repair the workflow artifact bundle before interpreting search quality.",
        "Rerun the hosted artifact workflow and require the full alpha-search artifact bundle.",
    ]


def feature_available(search_space: dict[str, Any], feature: str) -> bool:
    pool = search_space.get("feature_pool")
    return isinstance(pool, list) and feature in pool


def choose_feature(search_space: dict[str, Any], mutation_type: str) -> str | None:
    for feature in PREFERRED_FEATURES.get(mutation_type, []):
        if feature_available(search_space, feature):
            return feature
    return None


def mutation_from_node(node: dict[str, Any]) -> str:
    proposed = str(node.get("proposed_mutation") or "")
    if proposed in ALLOWED_MUTATIONS:
        return proposed
    dimension = str(node.get("selected_dimension") or "")
    return DIMENSION_TO_MUTATION.get(dimension, "clip_or_squash")


def build_prior(runs: list[dict[str, Any]], decision: dict[str, Any], limit: int) -> dict[str, Any]:
    current = latest_run(runs)
    mutations = []
    for node in selected_nodes(current["plan"])[:limit]:
        base_factor = str(node.get("factor_name") or "")
        if not base_factor:
            continue
        mutation_type = mutation_from_node(node)
        if mutation_type == "do_not_expand_collect_more_data":
            mutation_type = "add_feature_gate"
        item: dict[str, Any] = {
            "base_factor": base_factor,
            "mutation_type": mutation_type,
            "name": f"llm_{base_factor}_{mutation_type}",
        }
        feature = choose_feature(current["search_space"], mutation_type)
        if feature:
            if mutation_type == "replace_denominator":
                item["denominator_feature"] = feature
            else:
                item["feature"] = feature
        if mutation_type in {"replace_denominator", "add_spread_penalty"}:
            item["constant"] = 0.01
        if mutation_type == "clip_or_squash":
            item["lo"] = -3.0
            item["hi"] = 3.0
        if mutation_type == "change_time_window":
            item["window"] = 30
        mutations.append(item)

    if not mutations and decision["action"] == "revise_prior":
        mutations.append(
            {
                "base_factor": "auto_settlement_full_depth_settlement_edge",
                "mutation_type": "add_capacity_gate",
                "name": "llm_full_depth_edge_capacity_gate",
                "feature": "entry_capacity_score",
            }
        )

    return {
        "schema_version": 1,
        "kind": "typed_llm_prior_draft",
        "source": "alpha_search_closed_loop_agent",
        "target": current["target"],
        "decision_reason": decision["reason"],
        "mutations": mutations,
    }


def write_markdown(decision: dict[str, Any], path: Path, prior_path: Path | None) -> None:
    current = decision["current_run"]
    lines = [
        "# Alpha Search Closed-Loop Decision",
        "",
        f"- Action: `{decision['action']}`",
        f"- Reason: `{decision['reason']}`",
        f"- Allow chained dispatch: `{decision['allow_dispatch']}`",
        f"- Evidence stage: `{decision['evidence_stage']}`",
        f"- Target: `{decision['target']}`",
        f"- Current run: `{current.get('run_id') or 'n/a'}`",
        f"- Best candidate: `{current.get('best_candidate') or 'n/a'}`",
        f"- Best reward: `{current.get('best_reward') if current.get('best_reward') is not None else 'n/a'}`",
        f"- Handoff status: `{current.get('handoff_status') or 'n/a'}`",
        f"- Chain reason: `{current.get('chain_reason') or 'n/a'}`",
        "",
        decision["guarantee"],
        "",
        "## Next Steps",
        "",
    ]
    lines.extend(f"- {item}" for item in decision["next_steps"])
    if prior_path is not None:
        lines.extend(["", f"Typed prior draft: `{prior_path}`"])
    if decision["promotion_blockers"]:
        lines.extend(["", "## Promotion Blockers", ""])
        lines.extend(f"- `{item}`" for item in decision["promotion_blockers"][:20])
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact_dir", nargs="+", help="Downloaded alpha-search artifact directories")
    parser.add_argument("--target", default=DEFAULT_TARGET)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--output-prior-json")
    parser.add_argument("--prior-mutation-limit", type=int, default=6)
    args = parser.parse_args()

    runs = [load_artifact(Path(value), args.target) for value in args.artifact_dir]
    decision = closed_loop_decision(runs)

    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(decision, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    prior_path = Path(args.output_prior_json) if args.output_prior_json else None
    if prior_path is not None and decision["prior_revision_required"]:
        prior = build_prior(runs, decision, max(1, args.prior_mutation_limit))
        prior_path.parent.mkdir(parents=True, exist_ok=True)
        prior_path.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    elif prior_path is not None and prior_path.exists():
        prior_path.unlink()

    write_markdown(decision, Path(args.output_md), prior_path if decision["prior_revision_required"] else None)


if __name__ == "__main__":
    main()
