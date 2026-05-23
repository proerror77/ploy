#!/usr/bin/env python3
"""Turn a Research Manager plan artifact into bounded research follow-up actions."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
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


def _parse_symbols(raw: str) -> list[str]:
    return [item.strip() for item in raw.split(",") if item.strip()]


def _dispatch_gh_workflow(workflow: str, ref: str, fields: dict[str, str]) -> None:
    cmd = ["gh", "workflow", "run", workflow, "--ref", ref]
    for key, value in fields.items():
        cmd.extend(["-f", f"{key}={value}"])
    subprocess.run(cmd, check=True)


def _research_snapshot_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    symbols: str,
    stake_usd: str,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    start_date = _date_from_ts(market_data.get("dataset_start_ts"))
    end_date = _date_from_ts(market_data.get("dataset_end_ts"))
    options = {
        "data_profile": "pm5d-execution",
        "data_gate": "warn",
        "upload_full_snapshot": True,
    }
    blockers: list[str] = []
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    fields = {
        "git_ref": git_ref,
        "start_date": start_date,
        "end_date": end_date,
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
    }


def _walk_forward_dispatch(
    *,
    git_ref: str,
    snapshot_run_id: str,
    symbols: str,
    stake_usd: str,
    chain_remaining: int,
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


def build_executor_payload(args: argparse.Namespace, plan_payload: dict[str, Any]) -> dict[str, Any]:
    if plan_payload.get("schema_version") != "research_trace_plan.v1":
        raise SystemExit("research_trace_plan schema mismatch")
    plan = plan_payload.get("plan") or {}
    actions = [str(action) for action in plan.get("actions") or []]
    dispatches: list[dict[str, Any]] = []
    typed_prior: dict[str, Any] | None = None

    if plan.get("theme") == "fix_data" or any(
        action in {"rerun_snapshot_data_audit", "repair_or_exclude_missing_data_surface"}
        for action in actions
    ):
        dispatches.append(
            _research_snapshot_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                symbols=args.symbols,
                stake_usd=args.stake_usd,
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
            )
        )

    if plan.get("theme") == "revise_prior" or "generate_typed_llm_prior_json" in actions:
        typed_prior = {
            "schema_version": "research_manager_typed_prior.v1",
            "source": "research_trace_plan",
            "evidence_stage": plan.get("evidence_stage", ""),
            "theme": plan.get("theme", ""),
            "actions": actions,
            "constraints": [
                "reuse only runtime-contract-mappable inputs",
                "do not promote without runtime_market_update_replay evidence",
                "preserve one-event-one-decision accounting",
            ],
        }

    executable_dispatches = [item for item in dispatches if item["ready"]]
    blocked_dispatches = [item for item in dispatches if not item["ready"]]
    mode = "execute" if args.mode == "execute" and args.execute_ack == EXECUTE_ACK else "dry_run"
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
        },
        "dispatches": dispatches,
        "executable_dispatch_count": len(executable_dispatches),
        "blocked_dispatches": blocked_dispatches,
        "typed_prior": typed_prior,
        "side_effects_enabled": mode == "execute",
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
        lines.append(f"- `{item['workflow']}`: `{status}`; blockers: `{blockers}`")
    if payload.get("typed_prior"):
        lines.extend(["", "## Typed Prior", "", "- `research_manager_typed_prior.v1` generated"])
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
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    payload = build_executor_payload(args, _load_json(args.plan_json))
    _write_json(args.output_dir / "research-manager-executor.json", payload)
    (args.output_dir / "research-manager-executor.md").write_text(
        render_markdown(payload),
        encoding="utf-8",
    )
    if payload.get("typed_prior"):
        _write_json(args.output_dir / "next-llm-prior.json", payload["typed_prior"])

    if payload["side_effects_enabled"]:
        for item in payload["dispatches"]:
            if item["ready"]:
                _dispatch_gh_workflow(item["workflow"], args.git_ref, item["fields"])


if __name__ == "__main__":
    main()
