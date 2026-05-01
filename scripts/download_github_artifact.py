#!/usr/bin/env python3
"""Download and extract a GitHub Actions artifact by name.

This intentionally uses the GitHub REST API instead of actions/download-artifact
so downstream jobs can consume artifacts from previous workflow runs without
depending on the action's Node download path on self-hosted runners.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tempfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from typing import Any


def request_json(url: str, token: str) -> dict[str, Any]:
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode("utf-8"))


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):  # type: ignore[no-untyped-def]
        return None


def download_file(url: str, token: str, destination: Path) -> None:
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "Authorization": f"Bearer {token}",
            "X-GitHub-Api-Version": "2022-11-28",
        },
    )
    opener = urllib.request.build_opener(NoRedirectHandler)
    try:
        resp = opener.open(req, timeout=60)
    except urllib.error.HTTPError as err:
        if err.code not in {301, 302, 303, 307, 308} or "Location" not in err.headers:
            raise
        blob_url = err.headers["Location"]
        resp = urllib.request.urlopen(blob_url, timeout=300)

    with resp:
        with destination.open("wb") as handle:
            shutil.copyfileobj(resp, handle)


def safe_extract(zip_path: Path, output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    output_root = output_dir.resolve()
    with zipfile.ZipFile(zip_path) as archive:
        for member in archive.infolist():
            target = (output_dir / member.filename).resolve()
            if target != output_root and output_root not in target.parents:
                raise RuntimeError(f"refusing to extract path outside output dir: {member.filename}")
        archive.extractall(output_dir)


def copy_tree_contents(source_dir: Path, output_dir: Path) -> None:
    if not source_dir.is_dir():
        raise RuntimeError(f"strip prefix is not a directory in artifact: {source_dir}")

    output_dir.mkdir(parents=True, exist_ok=True)
    for item in source_dir.iterdir():
        target = output_dir / item.name
        if item.is_dir():
            if target.exists():
                shutil.rmtree(target)
            shutil.copytree(item, target)
        else:
            shutil.copy2(item, target)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", required=True, help="Workflow run id that owns the artifact")
    parser.add_argument("--name", required=True, help="Artifact name to download")
    parser.add_argument("--output-dir", required=True, help="Directory to extract the artifact into")
    parser.add_argument(
        "--strip-prefix",
        default="",
        help="Optional artifact subdirectory whose contents should become the output root",
    )
    parser.add_argument("--repo", default=os.environ.get("GITHUB_REPOSITORY"))
    parser.add_argument("--api-url", default=os.environ.get("GITHUB_API_URL", "https://api.github.com"))
    parser.add_argument("--token", default=os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN"))
    parser.add_argument(
        "--require",
        action="append",
        default=[],
        help="Relative path that must exist after extraction; may be repeated",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    if not args.repo:
        print("GITHUB_REPOSITORY is required or pass --repo", file=sys.stderr)
        return 2
    if not args.token:
        print("GITHUB_TOKEN or GH_TOKEN is required", file=sys.stderr)
        return 2

    artifacts_url = (
        f"{args.api_url}/repos/{args.repo}/actions/runs/{args.run_id}/artifacts"
        f"?name={args.name}&per_page=100"
    )
    try:
        payload = request_json(artifacts_url, args.token)
    except urllib.error.HTTPError as err:
        print(f"failed to list artifacts: HTTP {err.code}: {err.read().decode('utf-8')}", file=sys.stderr)
        return 1

    artifacts = [
        item
        for item in payload.get("artifacts", [])
        if item.get("name") == args.name and not item.get("expired", False)
    ]
    if not artifacts:
        found = ", ".join(item.get("name", "<unknown>") for item in payload.get("artifacts", []))
        print(f"artifact not found: {args.name}; found: {found or '<none>'}", file=sys.stderr)
        return 1

    artifact = sorted(artifacts, key=lambda item: item.get("created_at", ""))[-1]
    output_dir = Path(args.output_dir)
    with tempfile.TemporaryDirectory(prefix="ploy-artifact-") as tmp:
        zip_path = Path(tmp) / "artifact.zip"
        extract_dir = Path(tmp) / "extract"
        try:
            download_file(artifact["archive_download_url"], args.token, zip_path)
        except urllib.error.HTTPError as err:
            print(f"failed to download artifact: HTTP {err.code}: {err.read().decode('utf-8')}", file=sys.stderr)
            return 1
        if args.strip_prefix:
            safe_extract(zip_path, extract_dir)
            shutil.rmtree(output_dir, ignore_errors=True)
            copy_tree_contents(extract_dir / args.strip_prefix, output_dir)
        else:
            safe_extract(zip_path, output_dir)

    missing = [path for path in args.require if not (output_dir / path).exists()]
    if missing:
        print(f"artifact extracted but required paths are missing: {', '.join(missing)}", file=sys.stderr)
        return 1

    print(
        "downloaded artifact "
        f"name={artifact['name']} id={artifact['id']} size={artifact.get('size_in_bytes', 0)} "
        f"to={output_dir}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
