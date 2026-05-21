#!/usr/bin/env python3
"""Resolve the newest usable GitHub Actions artifact by prefix.

The daily Research OS workflow needs data-plane artifacts without requiring an
operator to paste run ids. This resolver lists retained repo artifacts, prefers
the requested branch when that metadata is available, optionally downloads a
small number of newest candidates to verify required files, then writes a JSON
record and optional GitHub environment variables.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sys
import tempfile
import urllib.error
from pathlib import Path
from typing import Any

try:
    from scripts.download_github_artifact import download_file, request_json, required_missing, safe_extract
except ModuleNotFoundError:  # pragma: no cover - script execution path
    from download_github_artifact import download_file, request_json, required_missing, safe_extract


def artifact_run_id(artifact: dict[str, Any]) -> str:
    workflow_run = artifact.get("workflow_run")
    if isinstance(workflow_run, dict) and workflow_run.get("id") is not None:
        return str(workflow_run["id"])
    match = re.search(r"(\d+)$", str(artifact.get("name") or ""))
    return match.group(1) if match else ""


def artifact_branch(artifact: dict[str, Any]) -> str:
    workflow_run = artifact.get("workflow_run")
    if isinstance(workflow_run, dict):
        return str(workflow_run.get("head_branch") or "").strip()
    return ""


def list_repo_artifacts(
    *,
    api_url: str,
    repo: str,
    token: str,
    max_pages: int,
) -> list[dict[str, Any]]:
    artifacts: list[dict[str, Any]] = []
    for page in range(1, max_pages + 1):
        url = f"{api_url}/repos/{repo}/actions/artifacts?per_page=100&page={page}"
        payload = request_json(url, token)
        page_items = payload.get("artifacts", [])
        if not isinstance(page_items, list) or not page_items:
            break
        artifacts.extend(item for item in page_items if isinstance(item, dict))
        if len(page_items) < 100:
            break
    return artifacts


def matching_artifacts(
    artifacts: list[dict[str, Any]],
    *,
    artifact_prefix: str,
    branch: str,
    fallback_any_branch: bool,
) -> list[dict[str, Any]]:
    candidates = [
        artifact
        for artifact in artifacts
        if str(artifact.get("name") or "").startswith(artifact_prefix)
        and not artifact.get("expired", False)
        and artifact_run_id(artifact)
    ]

    if branch:
        branch_candidates = [
            artifact
            for artifact in candidates
            if artifact_branch(artifact) in {"", branch}
        ]
        if branch_candidates or not fallback_any_branch:
            candidates = branch_candidates

    return sorted(candidates, key=lambda item: str(item.get("created_at") or ""), reverse=True)


def artifact_has_required_files(
    artifact: dict[str, Any],
    *,
    token: str,
    required_paths: list[str],
    strip_prefix: str,
) -> tuple[bool, list[str]]:
    if not required_paths:
        return True, []
    with tempfile.TemporaryDirectory(prefix="ploy-resolve-artifact-") as tmp:
        tmp_path = Path(tmp)
        zip_path = tmp_path / "artifact.zip"
        extract_dir = tmp_path / "extract"
        download_file(str(artifact["archive_download_url"]), token, zip_path)
        safe_extract(zip_path, extract_dir)
        root = extract_dir / strip_prefix if strip_prefix else extract_dir
        missing = required_missing(root, required_paths)
        return not missing, missing


def artifact_record(artifact: dict[str, Any], *, required_paths: list[str]) -> dict[str, Any]:
    workflow_run = artifact.get("workflow_run") if isinstance(artifact.get("workflow_run"), dict) else {}
    return {
        "found": True,
        "artifact_id": artifact.get("id"),
        "artifact_name": artifact.get("name"),
        "artifact_size": artifact.get("size_in_bytes", 0),
        "created_at": artifact.get("created_at"),
        "run_id": artifact_run_id(artifact),
        "head_branch": artifact_branch(artifact),
        "head_sha": workflow_run.get("head_sha") if isinstance(workflow_run, dict) else None,
        "required_paths": required_paths,
    }


def missing_record(*, artifact_prefix: str, branch: str, required_paths: list[str], reason: str) -> dict[str, Any]:
    return {
        "found": False,
        "artifact_prefix": artifact_prefix,
        "branch": branch,
        "required_paths": required_paths,
        "reason": reason,
    }


def write_json(path: str, payload: dict[str, Any]) -> None:
    if not path:
        return
    output = Path(path)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def write_env(path: str, prefix: str, payload: dict[str, Any]) -> None:
    if not path or not prefix:
        return
    env_prefix = re.sub(r"[^A-Za-z0-9_]+", "_", prefix).upper().strip("_")
    values = {
        f"{env_prefix}_FOUND": "true" if payload.get("found") else "false",
        f"{env_prefix}_RUN_ID": str(payload.get("run_id") or ""),
        f"{env_prefix}_ARTIFACT_NAME": str(payload.get("artifact_name") or ""),
        f"{env_prefix}_ARTIFACT_ID": str(payload.get("artifact_id") or ""),
    }
    with Path(path).open("a", encoding="utf-8") as handle:
        for key, value in values.items():
            if "\n" in value or "\r" in value:
                raise SystemExit(f"refusing to write multi-line env value for {key}")
            handle.write(f"{key}={value}\n")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifact-prefix", required=True)
    parser.add_argument("--branch", default="")
    parser.add_argument("--fallback-any-branch", action="store_true")
    parser.add_argument("--strip-prefix", default="")
    parser.add_argument("--require", action="append", default=[])
    parser.add_argument("--max-candidates", type=int, default=20)
    parser.add_argument("--max-pages", type=int, default=5)
    parser.add_argument("--allow-missing", action="store_true")
    parser.add_argument("--output-json", default="")
    parser.add_argument("--github-env", default="")
    parser.add_argument("--env-prefix", default="")
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--token", default=os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN"))
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not args.repo:
        print("GITHUB_REPOSITORY is required or pass --repo", file=sys.stderr)
        return 2
    if not args.token:
        print("GITHUB_TOKEN or GH_TOKEN is required", file=sys.stderr)
        return 2
    if args.max_candidates <= 0:
        print("--max-candidates must be positive", file=sys.stderr)
        return 2

    try:
        artifacts = list_repo_artifacts(
            api_url=args.api_url,
            repo=args.repo,
            token=args.token,
            max_pages=max(1, args.max_pages),
        )
    except urllib.error.HTTPError as err:
        print(f"failed to list repo artifacts: HTTP {err.code}: {err.read().decode('utf-8')}", file=sys.stderr)
        return 1

    candidates = matching_artifacts(
        artifacts,
        artifact_prefix=args.artifact_prefix,
        branch=args.branch,
        fallback_any_branch=args.fallback_any_branch,
    )

    last_missing: list[str] = []
    for artifact in candidates[: args.max_candidates]:
        try:
            ok, missing = artifact_has_required_files(
                artifact,
                token=args.token,
                required_paths=args.require,
                strip_prefix=args.strip_prefix,
            )
        except (urllib.error.HTTPError, RuntimeError, OSError, shutil.Error) as err:
            print(
                f"skipping artifact {artifact.get('name')} ({artifact_run_id(artifact)}): {err}",
                file=sys.stderr,
            )
            continue
        if not ok:
            last_missing = missing
            print(
                f"skipping artifact {artifact.get('name')} ({artifact_run_id(artifact)}): "
                f"missing {', '.join(missing)}",
                file=sys.stderr,
            )
            continue
        payload = artifact_record(artifact, required_paths=args.require)
        write_json(args.output_json, payload)
        write_env(args.github_env, args.env_prefix, payload)
        print(
            "resolved artifact "
            f"name={payload['artifact_name']} run_id={payload['run_id']} id={payload['artifact_id']}",
            flush=True,
        )
        return 0

    reason = "not_found" if not candidates else f"missing_required:{','.join(last_missing) or 'unknown'}"
    payload = missing_record(
        artifact_prefix=args.artifact_prefix,
        branch=args.branch,
        required_paths=args.require,
        reason=reason,
    )
    write_json(args.output_json, payload)
    write_env(args.github_env, args.env_prefix, payload)
    print(
        f"no usable artifact found for prefix {args.artifact_prefix}: {reason}",
        file=sys.stderr,
    )
    return 0 if args.allow_missing else 1


if __name__ == "__main__":
    raise SystemExit(main())
