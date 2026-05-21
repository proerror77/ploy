import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

from scripts import resolve_github_artifact as resolver


def artifact(
    name: str,
    *,
    artifact_id: int,
    created_at: str,
    run_id: int | None = None,
    branch: str = "",
    expired: bool = False,
) -> dict:
    workflow_run = {}
    if run_id is not None:
        workflow_run["id"] = run_id
    if branch:
        workflow_run["head_branch"] = branch
    return {
        "id": artifact_id,
        "name": name,
        "created_at": created_at,
        "expired": expired,
        "archive_download_url": f"https://example.invalid/artifacts/{artifact_id}.zip",
        "workflow_run": workflow_run,
    }


class ResolveGithubArtifactTests(unittest.TestCase):
    def test_prefers_newest_artifact_on_requested_branch(self) -> None:
        artifacts = [
            artifact("research-snapshot-100", artifact_id=1, created_at="2026-05-20T00:00:00Z", run_id=100, branch="main"),
            artifact("research-snapshot-200", artifact_id=2, created_at="2026-05-21T00:00:00Z", run_id=200, branch="feature"),
            artifact("research-snapshot-150", artifact_id=3, created_at="2026-05-20T12:00:00Z", run_id=150, branch="main"),
        ]

        matches = resolver.matching_artifacts(
            artifacts,
            artifact_prefix="research-snapshot-",
            branch="main",
            fallback_any_branch=True,
        )

        self.assertEqual([item["name"] for item in matches], ["research-snapshot-150", "research-snapshot-100"])

    def test_falls_back_to_any_branch_when_requested_branch_has_no_artifacts(self) -> None:
        artifacts = [
            artifact("research-snapshot-100", artifact_id=1, created_at="2026-05-20T00:00:00Z", run_id=100, branch="main"),
            artifact("research-snapshot-200", artifact_id=2, created_at="2026-05-21T00:00:00Z", run_id=200, branch="feature"),
        ]

        matches = resolver.matching_artifacts(
            artifacts,
            artifact_prefix="research-snapshot-",
            branch="release",
            fallback_any_branch=True,
        )

        self.assertEqual([item["name"] for item in matches], ["research-snapshot-200", "research-snapshot-100"])

    def test_ignores_expired_and_wrong_prefix_artifacts(self) -> None:
        artifacts = [
            artifact("research-snapshot-100", artifact_id=1, created_at="2026-05-20T00:00:00Z", run_id=100, expired=True),
            artifact("factor-walk-forward-v2-200", artifact_id=2, created_at="2026-05-21T00:00:00Z", run_id=200),
            artifact("research-snapshot-300", artifact_id=3, created_at="2026-05-22T00:00:00Z", run_id=300),
        ]

        matches = resolver.matching_artifacts(
            artifacts,
            artifact_prefix="research-snapshot-",
            branch="",
            fallback_any_branch=False,
        )

        self.assertEqual([item["name"] for item in matches], ["research-snapshot-300"])

    def test_falls_back_to_artifact_name_suffix_for_run_id(self) -> None:
        payload = artifact("autofactor-strategy-promotion-12345", artifact_id=9, created_at="2026-05-21T00:00:00Z")
        payload["workflow_run"] = {}

        self.assertEqual(resolver.artifact_run_id(payload), "12345")

    def test_validates_required_files_inside_downloaded_zip(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            zip_path = Path(tmp) / "artifact.zip"
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("manifest.json", "{}")
                archive.writestr("feature-snapshot-manifest.json", "{}")
                archive.writestr("quality.md", "ok")

            def copy_zip(_url: str, _token: str, destination: Path) -> None:
                destination.write_bytes(zip_path.read_bytes())

            payload = artifact("research-snapshot-123", artifact_id=1, created_at="2026-05-21T00:00:00Z", run_id=123)
            with mock.patch.object(resolver, "download_file", side_effect=copy_zip):
                ok, missing = resolver.artifact_has_required_files(
                    payload,
                    token="token",
                    required_paths=["manifest.json", "feature-snapshot-manifest.json", "quality.md"],
                    strip_prefix="",
                )

        self.assertTrue(ok)
        self.assertEqual(missing, [])

    def test_writes_github_env_with_empty_values_for_missing_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            env_path = Path(tmp) / "github_env"
            resolver.write_env(
                str(env_path),
                "resolved_snapshot",
                {"found": False},
            )

            self.assertEqual(
                env_path.read_text(encoding="utf-8").splitlines(),
                [
                    "RESOLVED_SNAPSHOT_FOUND=false",
                    "RESOLVED_SNAPSHOT_RUN_ID=",
                    "RESOLVED_SNAPSHOT_ARTIFACT_NAME=",
                    "RESOLVED_SNAPSHOT_ARTIFACT_ID=",
                ],
            )


if __name__ == "__main__":
    unittest.main()
