#!/usr/bin/env python3
"""Turn a Research Manager plan artifact into bounded research follow-up actions."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


EXECUTE_ACK = "execute-research-manager-plan"
SCHEMA_VERSION = "research_manager_executor.v1"


def _load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _date_from_ts(raw: str | None) -> str:
    if not raw:
        return "auto"
    return raw.split("T", 1)[0]


def _parse_utc_ts(raw: str | None) -> datetime | None:
    if not raw:
        return None
    parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _format_utc_ts(ts: datetime) -> str:
    return ts.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _bounded_snapshot_window(
    market_data: dict[str, Any],
    max_window_days: int,
) -> dict[str, Any]:
    start = _parse_utc_ts(market_data.get("dataset_start_ts"))
    end = _parse_utc_ts(market_data.get("dataset_end_ts"))
    if not start or not end or end <= start:
        return {
            "start_date": _date_from_ts(market_data.get("dataset_start_ts")),
            "end_date": _date_from_ts(market_data.get("dataset_end_ts")),
            "start_ts": market_data.get("dataset_start_ts") or "",
            "end_ts": market_data.get("dataset_end_ts") or "",
            "truncated": False,
            "max_window_days": max_window_days,
        }
    capped_end = min(end, start + timedelta(days=max_window_days))
    return {
        "start_date": start.date().isoformat(),
        "end_date": capped_end.date().isoformat(),
        "start_ts": _format_utc_ts(start),
        "end_ts": _format_utc_ts(capped_end),
        "truncated": capped_end < end,
        "max_window_days": max_window_days,
        "source_end_ts": _format_utc_ts(end),
    }


def _parse_symbols(raw: str) -> list[str]:
    return [item.strip() for item in raw.split(",") if item.strip()]


def _dispatch_gh_workflow(workflow: str, ref: str, fields: dict[str, str]) -> dict[str, Any]:
    cmd = ["gh", "workflow", "run", workflow, "--ref", ref]
    for key, value in fields.items():
        cmd.extend(["-f", f"{key}={value}"])
    completed = subprocess.run(cmd, text=True, capture_output=True)
    return {
        "workflow": workflow,
        "ref": ref,
        "ok": completed.returncode == 0,
        "returncode": completed.returncode,
        "stdout": completed.stdout.strip(),
        "stderr": completed.stderr.strip(),
    }


def _research_snapshot_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    symbols: str,
    stake_usd: str,
    max_snapshot_window_days: int,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_snapshot_window(
        market_data,
        max_window_days=max(1, max_snapshot_window_days),
    )
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "data_profile": "pm5d-execution",
        "data_gate": "warn",
        "upload_sampled_snapshot": True,
    }
    blockers: list[str] = []
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "symbols": symbols,
        "stake_usd": stake_usd,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "research-snapshot.yml",
        "reason": "refresh snapshot/data audit for Research Manager fix_data plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _bounded_execution_surface_window(
    market_data: dict[str, Any],
    max_hours: int,
) -> dict[str, Any]:
    start = _parse_utc_ts(market_data.get("dataset_start_ts"))
    end = _parse_utc_ts(market_data.get("dataset_end_ts"))
    max_hours = max(1, max_hours)
    if not start or not end or end <= start:
        return {
            "start_date": _date_from_ts(market_data.get("dataset_start_ts")),
            "end_date": _date_from_ts(market_data.get("dataset_end_ts")),
            "start_ts": market_data.get("dataset_start_ts") or "",
            "end_ts": market_data.get("dataset_end_ts") or "",
            "truncated": False,
            "max_hours": max_hours,
        }
    capped_end = min(end, start + timedelta(hours=max_hours))
    return {
        "start_date": start.date().isoformat(),
        "end_date": capped_end.date().isoformat(),
        "start_ts": _format_utc_ts(start),
        "end_ts": _format_utc_ts(capped_end),
        "truncated": capped_end < end,
        "max_hours": max_hours,
        "source_end_ts": _format_utc_ts(end),
    }


def _full_depth_execution_surface_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    max_hours: int,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_execution_surface_window(market_data, max_hours=max_hours)
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "max_hours": window["max_hours"],
        "fail_if_incomplete": False,
    }
    blockers: list[str] = []
    if not window["start_ts"] or not window["end_ts"]:
        blockers.append("missing_dataset_window")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "collect-full-depth-execution-surface.yml",
        "reason": "materialize full-fidelity Polymarket CLOB archive evidence for promotion surface",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _official_settlement_repair_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    symbols: str,
    execute: bool,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_snapshot_window(market_data, max_window_days=2)
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "mode": "execute" if execute else "dry_run",
    }
    blockers: list[str] = []
    if not window["start_ts"] or not window["end_ts"]:
        blockers.append("missing_dataset_window")
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "symbols": symbols,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
        "execute_ack": "repair-official-settlement-coverage" if execute else "",
    }
    return {
        "workflow": "repair-official-settlement-coverage.yml",
        "reason": "repair bounded official Polymarket settlement coverage for replay-traded events",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _walk_forward_dispatch(
    *,
    git_ref: str,
    snapshot_run_id: str,
    symbols: str,
    stake_usd: str,
    chain_remaining: int,
    alpha_search_plan_target: str = "",
    allowed_target: str = "",
    alpha_search_llm_prior: dict[str, Any] | None = None,
    candidate_strategy_replay_run_id: str = "",
    candidate_strategy_replay_artifact_name: str = "",
    full_depth_execution_surface_run_id: str = "",
    full_depth_execution_surface_artifact_name: str = "",
) -> dict[str, Any]:
    blockers: list[str] = []
    if not snapshot_run_id:
        blockers.append("missing_snapshot_run_id")
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    options = {
        "chain_next_run": chain_remaining > 0,
        "chain_remaining": max(chain_remaining - 1, 0),
        "create_config_pr": False,
        "create_handoff_issue": False,
        "fail_if_blocked": False,
        "persist_research_trace": True,
    }
    if alpha_search_plan_target:
        options["alpha_search_plan_target"] = alpha_search_plan_target
    if allowed_target:
        options["allowed_target"] = allowed_target
    if alpha_search_llm_prior:
        options["alpha_search_llm_prior_json"] = json.dumps(
            alpha_search_llm_prior,
            separators=(",", ":"),
            sort_keys=True,
        )
    if candidate_strategy_replay_run_id:
        options["candidate_strategy_replay_run_id"] = candidate_strategy_replay_run_id
    if candidate_strategy_replay_artifact_name:
        options["candidate_strategy_replay_artifact_name"] = candidate_strategy_replay_artifact_name
    if full_depth_execution_surface_run_id:
        options["full_depth_execution_surface_run_id"] = full_depth_execution_surface_run_id
    if full_depth_execution_surface_artifact_name:
        options["full_depth_execution_surface_artifact_name"] = full_depth_execution_surface_artifact_name
    fields = {
        "git_ref": git_ref,
        "snapshot_run_id": snapshot_run_id,
        "start_date": "auto",
        "end_date": "auto",
        "symbols": symbols,
        "stake_usd": stake_usd,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "factor-walk-forward-v2-hosted-artifact.yml",
        "reason": "bounded hosted walk-forward continuation from Research Manager plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }


def _runtime_candidate_replay_dispatch(
    *,
    deployment_id: str,
    config_path: str,
    recording_path: str,
    runtime_score: str,
    strategy_profile: str,
    issue_number: str,
    min_trade_count: str,
    min_fill_rate: str,
    min_roi: str,
    source_target: str,
    source_horizon: str,
) -> dict[str, Any]:
    blockers: list[str] = []
    if not runtime_score:
        blockers.append("missing_runtime_score")
    if not deployment_id:
        blockers.append("missing_deployment_id")
    if not config_path:
        blockers.append("missing_config_path")
    if not recording_path:
        blockers.append("missing_recording_path")
    if not strategy_profile:
        blockers.append("missing_strategy_profile")

    options = {
        "full_depth_entry": True,
        "skip_settlement_exits": False,
        "source_target": source_target,
        "source_horizon": source_horizon,
    }
    fields = {
        "deployment_id": deployment_id,
        "config_path": config_path,
        "recording_path": recording_path,
        "runtime_score": runtime_score,
        "strategy_profile": strategy_profile,
        "issue_number": issue_number,
        "min_trade_count": min_trade_count,
        "min_fill_rate": min_fill_rate,
        "min_roi": min_roi,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "runtime-candidate-replay.yml",
        "reason": "build runtime_market_update_replay evidence for Research Manager fix_runtime plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }


def _recorded_replay_parity_dispatch(
    *,
    deployment_id: str,
    config_path: str,
    recording_path: str,
    issue_number: str,
) -> dict[str, Any]:
    blockers: list[str] = []
    if not deployment_id:
        blockers.append("missing_deployment_id")
    if not config_path:
        blockers.append("missing_config_path")
    if not recording_path:
        blockers.append("missing_recording_path")

    fields = {
        "deployment_id": deployment_id,
        "config_path": config_path,
        "recording_path": recording_path,
        "since": "auto",
        "until": "auto",
        "issue_number": issue_number,
        "approval_environment": "tango-1-1-build-only",
        "skip_settlement_exits": "false",
        "runner_source": "deployed",
    }
    return {
        "workflow": "recorded-replay-parity.yml",
        "reason": "compare deployed runtime scorer decisions against recorded dry-run evidence",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if str(item)]


def _runtime_candidate_from_plan(
    plan_payload: dict[str, Any],
    *,
    strategy_profile: str,
    source_target: str,
    source_horizon: str,
) -> dict[str, str] | None:
    """Select the newest typed, unblocked runtime contract from the trace plan."""

    summary = ((plan_payload.get("input") or {}).get("factor_registry_summary") or {})
    recent = summary.get("recent_factors")
    if not isinstance(recent, list):
        return None
    for item in recent:
        if not isinstance(item, dict):
            continue
        status = str(item.get("status") or "")
        if status and status not in {"candidate", "watchlist"}:
            continue
        contract = item.get("runtime_contract")
        if not isinstance(contract, dict):
            continue
        blockers = set(_string_list(item.get("blockers"))) | set(
            _string_list(contract.get("blockers"))
        )
        if blockers:
            continue
        if contract.get("version") != "autofactor_runtime_contract_v1":
            continue
        runtime_score = str(contract.get("runtime_score") or "")
        contract_profile = str(contract.get("strategy_profile") or "")
        contract_target = str(contract.get("target") or item.get("target") or "")
        contract_horizon = str(contract.get("horizon") or item.get("horizon") or "")
        if not runtime_score or not contract_profile:
            continue
        if strategy_profile and contract_profile != strategy_profile:
            continue
        if source_target and contract_target and contract_target != source_target:
            continue
        if source_horizon and contract_horizon and contract_horizon != source_horizon:
            continue
        return {
            "factor_name": str(item.get("factor_name") or item.get("name") or ""),
            "runtime_score": runtime_score,
            "strategy_profile": contract_profile,
            "source_target": contract_target or source_target,
            "source_horizon": contract_horizon or source_horizon,
        }
    return None


def _latest_run(plan_payload: dict[str, Any]) -> dict[str, Any]:
    latest_runs = ((plan_payload.get("input") or {}).get("latest_runs") or {})
    runs = latest_runs.get("runs")
    if isinstance(runs, list) and runs and isinstance(runs[0], dict):
        return runs[0]
    return {}


def _latest_candidate_replay_contract(plan_payload: dict[str, Any]) -> dict[str, str]:
    latest_run = _latest_run(plan_payload)
    artifacts = latest_run.get("artifacts")
    if not isinstance(artifacts, list):
        return {}
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            continue
        output = artifact.get("output_json")
        if not isinstance(output, dict):
            continue
        replay = output.get("candidate_strategy_replay")
        if not isinstance(replay, dict):
            continue
        if replay.get("basis") != "runtime_market_update_replay":
            continue
        contract = replay.get("decision_contract")
        if not isinstance(contract, dict):
            contract = {}
        source_factor = replay.get("source_factor")
        if not isinstance(source_factor, dict):
            source_factor = {}
        return {
            "target": str(contract.get("target") or source_factor.get("target") or ""),
            "horizon": str(contract.get("horizon") or source_factor.get("horizon") or ""),
            "runtime_score": str(replay.get("runtime_score") or ""),
            "strategy_profile": str(replay.get("strategy_profile") or ""),
            "workflow_run_id": str(replay.get("workflow_run_id") or ""),
            "source_workflow": str(replay.get("source_workflow") or ""),
        }
    return {}


def _latest_valid_full_depth_surface_artifact(plan_payload: dict[str, Any]) -> dict[str, str]:
    latest_run = _latest_run(plan_payload)
    latest_run_id = str(latest_run.get("run_id") or latest_run.get("workflow_run_id") or "")
    if not latest_run_id:
        return {}
    artifacts = latest_run.get("artifacts")
    if not isinstance(artifacts, list):
        return {}
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            continue
        output = artifact.get("output_json")
        if not isinstance(output, dict):
            continue
        snapshot_contract = output.get("source_snapshot_contract")
        if not isinstance(snapshot_contract, dict):
            continue
        proofs = snapshot_contract.get("full_depth_execution_surface_proofs")
        if not isinstance(proofs, list):
            continue
        for proof in proofs:
            if not isinstance(proof, dict):
                continue
            if proof.get("valid") is not True:
                continue
            if proof.get("surface") != "clob_orderbook_snapshots":
                continue
            blockers = proof.get("blockers")
            if isinstance(blockers, list) and blockers:
                continue
            return {
                "run_id": latest_run_id,
                "artifact_name": f"factor-walk-forward-v2-{latest_run_id}",
            }
    return {}


def _runtime_replay_args(args: argparse.Namespace, plan_payload: dict[str, Any]) -> dict[str, Any]:
    selected = _runtime_candidate_from_plan(
        plan_payload,
        strategy_profile=args.runtime_strategy_profile,
        source_target=args.runtime_source_target,
        source_horizon=args.runtime_source_horizon,
    )
    return {
        "runtime_score": args.runtime_score or (selected or {}).get("runtime_score", ""),
        "strategy_profile": args.runtime_strategy_profile
        or (selected or {}).get("strategy_profile", ""),
        "source_target": (selected or {}).get("source_target") or args.runtime_source_target,
        "source_horizon": (selected or {}).get("source_horizon") or args.runtime_source_horizon,
        "selected_factor_name": (selected or {}).get("factor_name", ""),
        "extra_blockers": [] if args.runtime_score or selected else ["missing_runtime_candidate_contract"],
    }


def _blocker_actions(plan: dict[str, Any]) -> list[dict[str, str]]:
    raw = plan.get("blocker_actions")
    if not isinstance(raw, list):
        return []
    actions: list[dict[str, str]] = []
    for item in raw:
        if not isinstance(item, dict):
            continue
        blocker_family = str(item.get("blocker_family") or "")
        action = str(item.get("action") or "")
        reason = str(item.get("reason") or "")
        if blocker_family and action:
            actions.append(
                {
                    "blocker_family": blocker_family,
                    "action": action,
                    "reason": reason,
                }
            )
    return actions


def _typed_prior_constraints(blocker_actions: list[dict[str, str]]) -> list[str]:
    constraints = [
        "reuse only runtime-contract-mappable inputs",
        "do not promote without runtime_market_update_replay evidence",
        "preserve one-event-one-decision accounting",
    ]
    action_names = {item["action"] for item in blocker_actions}
    if "increase_distinct_event_coverage_or_reduce_selectivity" in action_names:
        constraints.append("prefer candidates with broader distinct-event coverage and avoid ultra-narrow buckets")
    if "prefer_high_fillability_depth_filters" in action_names:
        constraints.append("prefer candidates with stronger full-depth fillability and capacity filters")
    if "collect_full_depth_execution_surface" in action_names:
        constraints.append("block promotion until full-depth execution-surface evidence replaces sampled snapshots")
    if "repair_official_settlement_coverage" in action_names:
        constraints.append("block promotion until official settlement coverage exists for all replay-traded events")
    if "repair_runtime_contract_mapping" in action_names:
        constraints.append("only select factors with typed unblocked runtime contracts")
    return constraints


def build_executor_payload(args: argparse.Namespace, plan_payload: dict[str, Any]) -> dict[str, Any]:
    if plan_payload.get("schema_version") != "research_trace_plan.v1":
        raise SystemExit("research_trace_plan schema mismatch")
    plan = plan_payload.get("plan") or {}
    actions = [str(action) for action in plan.get("actions") or []]
    blocker_actions = _blocker_actions(plan)
    dispatches: list[dict[str, Any]] = []
    typed_prior: dict[str, Any] | None = None
    side_effect_mode = args.mode == "execute" and args.execute_ack == EXECUTE_ACK

    action_names = {item["action"] for item in blocker_actions}
    snapshot_actions = {"rerun_snapshot_data_audit", "repair_or_exclude_missing_data_surface"}
    latest_replay_contract = _latest_candidate_replay_contract(plan_payload)
    latest_full_depth_surface_artifact = _latest_valid_full_depth_surface_artifact(plan_payload)
    walk_forward_target = latest_replay_contract.get("target") or getattr(
        args,
        "runtime_source_target",
        "full_depth_settlement_executable_pnl",
    )

    if (
        plan.get("theme") == "revise_prior"
        or "generate_typed_llm_prior_json" in actions
        or blocker_actions
    ):
        typed_prior = {
            "schema_version": "research_manager_typed_prior.v1",
            "source": "research_trace_plan",
            "evidence_stage": plan.get("evidence_stage", ""),
            "theme": plan.get("theme", ""),
            "actions": actions,
            "blocker_actions": blocker_actions,
            "constraints": _typed_prior_constraints(blocker_actions),
        }

    if any(action in snapshot_actions for action in actions) or (
        plan.get("theme") == "fix_data" and not actions
    ):
        dispatches.append(
            _research_snapshot_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                symbols=args.symbols,
                stake_usd=args.stake_usd,
                max_snapshot_window_days=args.max_snapshot_window_days,
            )
        )

    if "collect_full_depth_execution_surface" in actions or (
        "collect_full_depth_execution_surface" in action_names
    ):
        dispatches.append(
            _full_depth_execution_surface_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                max_hours=getattr(args, "max_full_depth_surface_hours", 12),
            )
        )

    if "repair_official_settlement_coverage" in actions or (
        "repair_official_settlement_coverage" in action_names
    ):
        dispatches.append(
            _official_settlement_repair_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                symbols=args.symbols,
                execute=side_effect_mode,
            )
        )

    if any(
        action in {"continue_hosted_alpha_search", "rerun_alpha_search_with_bounded_mutations"}
        for action in actions
    ):
        dispatches.append(
            _walk_forward_dispatch(
                git_ref=args.git_ref,
                snapshot_run_id=args.snapshot_run_id,
                symbols=args.symbols,
                stake_usd=args.stake_usd,
                chain_remaining=args.chain_remaining,
                alpha_search_plan_target=walk_forward_target,
                allowed_target=walk_forward_target,
                alpha_search_llm_prior=typed_prior,
                candidate_strategy_replay_run_id=getattr(
                    args,
                    "candidate_strategy_replay_run_id",
                    "",
                )
                or latest_replay_contract.get("workflow_run_id", ""),
                candidate_strategy_replay_artifact_name=getattr(
                    args,
                    "candidate_strategy_replay_artifact_name",
                    "",
                )
                or (
                    f"runtime-candidate-replay-{latest_replay_contract['workflow_run_id']}"
                    if latest_replay_contract.get("workflow_run_id")
                    and latest_replay_contract.get("source_workflow")
                    == "runtime-candidate-replay.yml"
                    else ""
                ),
                full_depth_execution_surface_run_id=getattr(
                    args,
                    "full_depth_execution_surface_run_id",
                    "",
                )
                or latest_full_depth_surface_artifact.get("run_id", ""),
                full_depth_execution_surface_artifact_name=getattr(
                    args,
                    "full_depth_execution_surface_artifact_name",
                    "",
                )
                or latest_full_depth_surface_artifact.get("artifact_name", ""),
            )
        )

    if "compare_runtime_scorer_contract" in actions or "build_runtime_candidate_replay" in actions:
        replay_args = _runtime_replay_args(args, plan_payload)
        dispatches.append(
            _runtime_candidate_replay_dispatch(
                deployment_id=args.runtime_deployment_id,
                config_path=args.runtime_config_path,
                recording_path=args.runtime_recording_path,
                runtime_score=replay_args["runtime_score"],
                strategy_profile=replay_args["strategy_profile"],
                issue_number=args.runtime_issue_number,
                min_trade_count=args.runtime_min_trade_count,
                min_fill_rate=args.runtime_min_fill_rate,
                min_roi=args.runtime_min_roi,
                source_target=replay_args["source_target"],
                source_horizon=replay_args["source_horizon"],
            )
        )
        if replay_args["extra_blockers"]:
            dispatches[-1]["blockers"].extend(replay_args["extra_blockers"])
            dispatches[-1]["blockers"] = sorted(set(dispatches[-1]["blockers"]))
            dispatches[-1]["ready"] = False
        if replay_args["selected_factor_name"]:
            dispatches[-1]["selected_factor_name"] = replay_args["selected_factor_name"]

    if "run_recorded_replay_parity" in actions:
        dispatches.append(
            _recorded_replay_parity_dispatch(
                deployment_id=args.runtime_deployment_id,
                config_path=args.runtime_config_path,
                recording_path=args.runtime_recording_path,
                issue_number=args.runtime_issue_number,
            )
        )

    executable_dispatches = [item for item in dispatches if item["ready"]]
    blocked_dispatches = [item for item in dispatches if not item["ready"]]
    mode = "execute" if side_effect_mode else "dry_run"
    if args.mode == "execute" and args.execute_ack != EXECUTE_ACK:
        blocked_dispatches.append(
            {
                "workflow": "<all>",
                "reason": "execute mode requested without ACK",
                "ready": False,
                "blockers": ["missing_execute_ack"],
                "fields": {},
            }
        )

    return {
        "schema_version": SCHEMA_VERSION,
        "mode": mode,
        "source_plan": {
            "schema_version": plan_payload.get("schema_version"),
            "theme": plan.get("theme", ""),
            "priority": plan.get("priority", ""),
            "evidence_stage": plan.get("evidence_stage", ""),
            "actions": actions,
            "blocker_actions": blocker_actions,
        },
        "dispatches": dispatches,
        "executable_dispatch_count": len(executable_dispatches),
        "blocked_dispatches": blocked_dispatches,
        "typed_prior": typed_prior,
        "side_effects_enabled": mode == "execute",
        "dispatch_attempts": [],
    }


def render_markdown(payload: dict[str, Any]) -> str:
    lines = [
        "# Research Manager Executor",
        "",
        f"- Mode: `{payload['mode']}`",
        f"- Source theme: `{payload['source_plan']['theme']}`",
        f"- Source evidence stage: `{payload['source_plan']['evidence_stage']}`",
        f"- Executable dispatches: `{payload['executable_dispatch_count']}`",
        "",
        "## Dispatch Plan",
        "",
    ]
    if not payload["dispatches"]:
        lines.append("- `<none>`")
    for item in payload["dispatches"]:
        status = "ready" if item["ready"] else "blocked"
        blockers = ", ".join(item["blockers"]) if item["blockers"] else "<none>"
        selected = item.get("selected_factor_name")
        suffix = f"; selected factor: `{selected}`" if selected else ""
        lines.append(f"- `{item['workflow']}`: `{status}`; blockers: `{blockers}`{suffix}")
    if payload.get("typed_prior"):
        lines.extend(["", "## Typed Prior", "", "- `research_manager_typed_prior.v1` generated"])
        blocker_actions = payload["typed_prior"].get("blocker_actions") or []
        if blocker_actions:
            lines.extend(["", "### Blocker Actions", ""])
            for item in blocker_actions:
                lines.append(f"- `{item['blocker_family']}` -> `{item['action']}`")
    if payload.get("dispatch_attempts"):
        lines.extend(["", "## Dispatch Attempts", ""])
        for item in payload["dispatch_attempts"]:
            status = "ok" if item.get("ok") else "failed"
            lines.append(f"- `{item['workflow']}`: `{status}`")
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-json", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--mode", choices=["dry_run", "execute"], default="dry_run")
    parser.add_argument("--execute-ack", default="")
    parser.add_argument("--git-ref", default="main")
    parser.add_argument("--snapshot-run-id", default="")
    parser.add_argument("--symbols", default="")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument("--chain-remaining", type=int, default=1)
    parser.add_argument("--candidate-strategy-replay-run-id", default="")
    parser.add_argument("--candidate-strategy-replay-artifact-name", default="")
    parser.add_argument("--full-depth-execution-surface-run-id", default="")
    parser.add_argument("--full-depth-execution-surface-artifact-name", default="")
    parser.add_argument("--max-snapshot-window-days", type=int, default=2)
    parser.add_argument("--max-full-depth-surface-hours", type=int, default=12)
    parser.add_argument(
        "--runtime-deployment-id",
        default="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
    )
    parser.add_argument(
        "--runtime-config-path",
        default="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
    )
    parser.add_argument(
        "--runtime-recording-path",
        default="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
    )
    parser.add_argument(
        "--runtime-score",
        default="",
    )
    parser.add_argument("--runtime-strategy-profile", default="settlement_probability")
    parser.add_argument("--runtime-issue-number", default="538")
    parser.add_argument("--runtime-min-trade-count", default="50")
    parser.add_argument("--runtime-min-fill-rate", default="0.30")
    parser.add_argument("--runtime-min-roi", default="0")
    parser.add_argument("--runtime-source-target", default="full_depth_settlement_executable_pnl")
    parser.add_argument("--runtime-source-horizon", default="5m")
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    payload = build_executor_payload(args, _load_json(args.plan_json))
    if payload["side_effects_enabled"]:
        attempts = []
        for item in payload["dispatches"]:
            if item["ready"]:
                attempts.append(_dispatch_gh_workflow(item["workflow"], args.git_ref, item["fields"]))
        payload["dispatch_attempts"] = attempts

    _write_json(args.output_dir / "research-manager-executor.json", payload)
    (args.output_dir / "research-manager-executor.md").write_text(
        render_markdown(payload),
        encoding="utf-8",
    )
    if payload.get("typed_prior"):
        _write_json(args.output_dir / "next-llm-prior.json", payload["typed_prior"])


if __name__ == "__main__":
    main()
