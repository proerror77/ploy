#!/usr/bin/env python3
"""Orchestrate the settlement-probability PRD evidence gate.

The gate is intentionally strict:

1. Build a portable pm5d-vol research snapshot with a critical data audit.
2. Feed that snapshot into Factor Walk-Forward V2.
3. Optionally attach a replay/dry-run parity artifact to the promotion gate.

This script is a control-plane helper around existing GitHub Actions workflows;
it does not run database research locally.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from typing import Any


RESEARCH_SNAPSHOT_WORKFLOW = "research-snapshot.yml"
WALK_FORWARD_WORKFLOW = "factor-walk-forward-v2.yml"


@dataclass(frozen=True)
class WorkflowRun:
    database_id: int
    status: str
    conclusion: str | None
    url: str
    created_at: datetime


def run_command(args: list[str], *, dry_run: bool = False) -> str:
    print("+ " + " ".join(args), flush=True)
    if dry_run:
        return ""
    completed = subprocess.run(
        args,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if completed.stderr.strip():
        print(completed.stderr.strip(), file=sys.stderr)
    return completed.stdout


def run_json(args: list[str]) -> Any:
    output = run_command(args)
    return json.loads(output) if output.strip() else None


def git_branch() -> str:
    try:
        value = run_command(["git", "branch", "--show-current"]).strip()
    except subprocess.CalledProcessError:
        value = ""
    return value or "main"


def parse_created_at(value: str) -> datetime:
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def find_latest_run(workflow: str, created_after: datetime) -> WorkflowRun:
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        rows = run_json(
            [
                "gh",
                "run",
                "list",
                "--workflow",
                workflow,
                "--event",
                "workflow_dispatch",
                "--limit",
                "50",
                "--json",
                "databaseId,status,conclusion,url,createdAt",
            ]
        )
        candidates = []
        for row in rows or []:
            created_at = parse_created_at(row["createdAt"])
            if created_at >= created_after - timedelta(seconds=5):
                candidates.append(
                    WorkflowRun(
                        database_id=int(row["databaseId"]),
                        status=str(row["status"]),
                        conclusion=row.get("conclusion"),
                        url=str(row["url"]),
                        created_at=created_at,
                    )
                )
        if candidates:
            return sorted(candidates, key=lambda run: run.created_at)[-1]
        time.sleep(5)
    raise RuntimeError(f"could not find dispatched run for {workflow}")


def refresh_run(run_id: int) -> WorkflowRun:
    row = run_json(
        [
            "gh",
            "run",
            "view",
            str(run_id),
            "--json",
            "databaseId,status,conclusion,url,createdAt",
        ]
    )
    return WorkflowRun(
        database_id=int(row["databaseId"]),
        status=str(row["status"]),
        conclusion=row.get("conclusion"),
        url=str(row["url"]),
        created_at=parse_created_at(row["createdAt"]),
    )


def wait_for_run(run: WorkflowRun, *, timeout_minutes: int, poll_seconds: int) -> WorkflowRun:
    deadline = time.monotonic() + timeout_minutes * 60
    current = run
    while current.status != "completed":
        if time.monotonic() >= deadline:
            raise TimeoutError(f"timed out waiting for run {run.database_id}: {run.url}")
        print(
            f"waiting run={current.database_id} status={current.status} url={current.url}",
            flush=True,
        )
        time.sleep(poll_seconds)
        current = refresh_run(run.database_id)
    print(
        f"completed run={current.database_id} conclusion={current.conclusion} url={current.url}",
        flush=True,
    )
    return current


def dispatch_workflow(
    workflow: str,
    fields: dict[str, str],
    *,
    workflow_ref: str,
    dry_run: bool,
) -> WorkflowRun | None:
    marker = datetime.now(timezone.utc)
    args = ["gh", "workflow", "run", workflow, "--ref", workflow_ref]
    for key, value in fields.items():
        args.extend(["-f", f"{key}={value}"])
    run_command(args, dry_run=dry_run)
    if dry_run:
        return None
    return find_latest_run(workflow, marker)


def issue_comment(issue_number: str, body: str, *, dry_run: bool) -> None:
    if not issue_number:
        return
    run_command(["gh", "issue", "comment", issue_number, "--body", body], dry_run=dry_run)


def compact_json(payload: dict[str, Any]) -> str:
    return json.dumps(payload, separators=(",", ":"), sort_keys=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--git-ref", default="", help="Branch/SHA to dispatch; defaults to current branch")
    parser.add_argument("--start-date", required=True, help="Research window start date, YYYY-MM-DD")
    parser.add_argument("--end-date", required=True, help="Research window end date, YYYY-MM-DD")
    parser.add_argument("--symbols", default="BTCUSDT,ETHUSDT,SOLUSDT")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument("--issue-number", default="321")
    parser.add_argument("--audit-lookback-hours", default="168")
    parser.add_argument("--lob-sample-secs", default="30")
    parser.add_argument("--observation-sample-secs", default="30")
    parser.add_argument("--max-quote-age-secs", default="30")
    parser.add_argument("--train-window-days", default="2")
    parser.add_argument("--test-window-days", default="1")
    parser.add_argument("--step-days", default="1")
    parser.add_argument("--min-observations", default="20")
    parser.add_argument("--top-quantile", default="0.2")
    parser.add_argument("--top-n", default="20")
    parser.add_argument("--replay-parity-run-id", default="")
    parser.add_argument("--replay-parity-artifact-name", default="")
    parser.add_argument("--snapshot-timeout-minutes", type=int, default=180)
    parser.add_argument("--walk-timeout-minutes", type=int, default=120)
    parser.add_argument("--poll-seconds", type=int, default=30)
    parser.add_argument("--no-wait", action="store_true", help="Dispatch the snapshot only and return")
    parser.add_argument("--dry-run", action="store_true", help="Print workflow dispatches without running them")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    git_ref = args.git_ref or git_branch()

    snapshot_options = {
        "lob_sample_secs": int(args.lob_sample_secs),
        "observation_sample_secs": int(args.observation_sample_secs),
        "max_quote_age_secs": int(args.max_quote_age_secs),
        "optimizer_data_dir": "/tmp/ploy-parquet",
        "data_profile": "pm5d-vol",
        "custom_required_sources": "",
        "audit_lookback_hours": int(args.audit_lookback_hours),
        "data_gate": "critical",
        "snapshot_registry_dir": "",
        "upload_full_snapshot": True,
    }
    snapshot_fields = {
        "git_ref": git_ref,
        "start_date": args.start_date,
        "end_date": args.end_date,
        "symbols": args.symbols,
        "stake_usd": args.stake_usd,
        "options_json": compact_json(snapshot_options),
    }

    print("Dispatching strict pm5d-vol research snapshot gate.", flush=True)
    snapshot_run = dispatch_workflow(
        RESEARCH_SNAPSHOT_WORKFLOW,
        snapshot_fields,
        workflow_ref=git_ref,
        dry_run=args.dry_run,
    )
    if args.dry_run:
        return 0
    if snapshot_run is None:
        raise RuntimeError("snapshot dispatch did not return a run")
    print(f"snapshot_run_id={snapshot_run.database_id} url={snapshot_run.url}", flush=True)
    if args.no_wait:
        return 0

    snapshot_result = wait_for_run(
        snapshot_run,
        timeout_minutes=args.snapshot_timeout_minutes,
        poll_seconds=args.poll_seconds,
    )
    if snapshot_result.conclusion != "success":
        issue_comment(
            args.issue_number,
            "\n".join(
                [
                    "Settlement probability PRD gate blocked at strict snapshot:",
                    "",
                    f"- Snapshot run: {snapshot_result.url}",
                    f"- Conclusion: `{snapshot_result.conclusion}`",
                    f"- Data profile: `pm5d-vol`",
                    f"- Data gate: `critical`",
                    f"- Lookback: `{args.audit_lookback_hours}h`",
                    "- Decision: wait for missing retained data / collector coverage before promotion.",
                ]
            ),
            dry_run=False,
        )
        return 1

    walk_options = {
        "train_window_days": int(args.train_window_days),
        "test_window_days": int(args.test_window_days),
        "step_days": int(args.step_days),
        "lob_sample_secs": int(args.lob_sample_secs),
        "observation_sample_secs": int(args.observation_sample_secs),
        "max_quote_age_secs": int(args.max_quote_age_secs),
        "top_n": int(args.top_n),
        "min_observations": int(args.min_observations),
        "top_quantile": float(args.top_quantile),
        "factor_name_filter": "",
        "compile_snapshot": False,
        "allow_direct_db_debug": False,
        "optimizer_data_dir": "/tmp/ploy-parquet",
        "data_profile": "pm5d-vol",
        "custom_required_sources": "",
        "audit_lookback_hours": int(args.audit_lookback_hours),
        "data_gate": "critical",
        "issue_number": args.issue_number,
        "snapshot_registry_dir": "",
        "replay_parity_json": "",
        "replay_parity_run_id": args.replay_parity_run_id,
        "replay_parity_artifact_name": args.replay_parity_artifact_name,
    }
    walk_fields = {
        "git_ref": git_ref,
        "start_date": args.start_date,
        "end_date": args.end_date,
        "symbols": args.symbols,
        "stake_usd": args.stake_usd,
        "snapshot_run_id": str(snapshot_result.database_id),
        "options_json": compact_json(walk_options),
    }

    print("Dispatching settlement probability promotion gate.", flush=True)
    walk_run = dispatch_workflow(
        WALK_FORWARD_WORKFLOW,
        walk_fields,
        workflow_ref=git_ref,
        dry_run=False,
    )
    if walk_run is None:
        raise RuntimeError("walk-forward dispatch did not return a run")
    print(f"walk_forward_run_id={walk_run.database_id} url={walk_run.url}", flush=True)
    walk_result = wait_for_run(
        walk_run,
        timeout_minutes=args.walk_timeout_minutes,
        poll_seconds=args.poll_seconds,
    )

    body = "\n".join(
        [
            "Settlement probability PRD gate orchestration completed:",
            "",
            f"- Snapshot run: {snapshot_result.url}",
            f"- Walk-forward run: {walk_result.url}",
            f"- Walk-forward conclusion: `{walk_result.conclusion}`",
            f"- Replay parity run supplied: `{args.replay_parity_run_id or 'none'}`",
            "- Decision: inspect the `Settlement Probability PRD Promotion Gate` section before any dry-run handoff.",
        ]
    )
    issue_comment(args.issue_number, body, dry_run=False)
    return 0 if walk_result.conclusion == "success" else 1


if __name__ == "__main__":
    raise SystemExit(main())
