use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const DEPLOYMENTS_FILE_NAME: &str = "deployments.json";
const DEPLOYMENTS_FILE_ENV: &str = "PLOY_DEPLOYMENTS_FILE";
const ALLOW_UNSAFE_DEPLOYMENTS_FILE_ENV: &str = "PLOY_ALLOW_UNSAFE_DEPLOYMENTS_FILE";

fn parse_boolish(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn allow_unsafe_deployments_file_override() -> bool {
    std::env::var(ALLOW_UNSAFE_DEPLOYMENTS_FILE_ENV)
        .ok()
        .map(|raw| parse_boolish(&raw))
        .unwrap_or(false)
}

fn contains_parent_dir(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn repo_allowed_relative_paths() -> [PathBuf; 2] {
    [
        PathBuf::from("data/state/deployments.json"),
        PathBuf::from("deployment/deployments.json"),
    ]
}

fn allowed_absolute_override_paths(cwd: &Path) -> [PathBuf; 4] {
    [
        cwd.join("data/state/deployments.json"),
        cwd.join("deployment/deployments.json"),
        PathBuf::from("/opt/ploy/data/state/deployments.json"),
        PathBuf::from("/opt/ploy/deployment/deployments.json"),
    ]
}

fn validate_override_file_kind(candidate: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(candidate) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(format!(
                "failed to inspect override {}: {}",
                candidate.display(),
                err
            ));
        }
    };

    if metadata.file_type().is_symlink() {
        return Err(format!(
            "override {} must not be a symlink",
            candidate.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "override {} must point to a regular file",
            candidate.display()
        ));
    }
    Ok(())
}

fn resolve_deployments_override(raw: &str, cwd: &Path, allow_unsafe: bool) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("override path is empty".to_string());
    }

    let candidate = PathBuf::from(trimmed);
    if contains_parent_dir(&candidate) {
        return Err(format!(
            "override {} contains parent traversal",
            candidate.display()
        ));
    }

    if candidate.file_name() != Some(OsStr::new(DEPLOYMENTS_FILE_NAME)) {
        return Err(format!(
            "override {} must target {}",
            candidate.display(),
            DEPLOYMENTS_FILE_NAME
        ));
    }

    if allow_unsafe {
        validate_override_file_kind(&candidate)?;
        return Ok(candidate);
    }

    let allowed = if candidate.is_absolute() {
        allowed_absolute_override_paths(cwd)
            .into_iter()
            .any(|allowed| candidate == allowed)
    } else {
        repo_allowed_relative_paths()
            .into_iter()
            .any(|allowed| candidate == allowed)
    };

    if !allowed {
        return Err(format!(
            "override {} is outside the supported deployments roots",
            candidate.display()
        ));
    }

    validate_override_file_kind(&candidate)?;
    Ok(candidate)
}

pub(crate) fn deployments_state_path() -> PathBuf {
    if let Ok(raw_override) = std::env::var(DEPLOYMENTS_FILE_ENV) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let allow_unsafe = allow_unsafe_deployments_file_override();
        match resolve_deployments_override(&raw_override, &cwd, allow_unsafe) {
            Ok(path) => {
                if allow_unsafe {
                    tracing::warn!(
                        override_path = %path.display(),
                        env = ALLOW_UNSAFE_DEPLOYMENTS_FILE_ENV,
                        "allowing unsafe deployments file override; prefer a supported state path"
                    );
                }
                return path;
            }
            Err(err) => {
                tracing::warn!(
                    override_path = raw_override.as_str(),
                    env = DEPLOYMENTS_FILE_ENV,
                    error = %err,
                    "ignoring unsafe deployments file override"
                );
            }
        }
    }

    let container_data_root = Path::new("/opt/ploy/data");
    if container_data_root.exists() {
        return container_data_root.join("state/deployments.json");
    }

    for candidate in repo_allowed_relative_paths() {
        if candidate.exists() {
            return candidate;
        }
    }

    let container_deployment = Path::new("/opt/ploy/deployment/deployments.json");
    if container_deployment.exists() {
        return container_deployment.to_path_buf();
    }

    PathBuf::from("data/state/deployments.json")
}

pub(crate) fn deployment_file_candidates(primary: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for candidate in [
        primary.to_path_buf(),
        PathBuf::from("data/state/deployments.json"),
        PathBuf::from("/opt/ploy/data/state/deployments.json"),
        PathBuf::from("deployment/deployments.json"),
        PathBuf::from("/opt/ploy/deployment/deployments.json"),
    ] {
        if !out.iter().any(|existing| existing == &candidate) {
            out.push(candidate);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{deployment_file_candidates, resolve_deployments_override};
    use std::path::{Path, PathBuf};

    #[test]
    fn accepts_repo_relative_deployments_override() {
        let cwd = Path::new("/repo");
        let resolved =
            resolve_deployments_override("data/state/deployments.json", cwd, false).unwrap();
        assert_eq!(resolved, PathBuf::from("data/state/deployments.json"));
    }

    #[test]
    fn accepts_repo_absolute_deployments_override() {
        let cwd = Path::new("/repo");
        let resolved = resolve_deployments_override(
            "/repo/data/state/deployments.json",
            cwd,
            false,
        )
        .unwrap();
        assert_eq!(resolved, PathBuf::from("/repo/data/state/deployments.json"));
    }

    #[test]
    fn rejects_parent_traversal_override() {
        let cwd = Path::new("/repo");
        let err = resolve_deployments_override("../../etc/passwd", cwd, false).unwrap_err();
        assert!(err.contains("parent traversal"));
    }

    #[test]
    fn rejects_wrong_basename_override() {
        let cwd = Path::new("/repo");
        let err = resolve_deployments_override("/repo/data/state/secrets.txt", cwd, false)
            .unwrap_err();
        assert!(err.contains("must target deployments.json"));
    }

    #[test]
    fn rejects_absolute_path_outside_supported_roots_without_escape_hatch() {
        let cwd = Path::new("/repo");
        let err =
            resolve_deployments_override("/tmp/deployments.json", cwd, false).unwrap_err();
        assert!(err.contains("outside the supported deployments roots"));
    }

    #[test]
    fn unsafe_escape_hatch_allows_nonstandard_path() {
        let cwd = Path::new("/repo");
        let resolved =
            resolve_deployments_override("/tmp/deployments.json", cwd, true).unwrap();
        assert_eq!(resolved, PathBuf::from("/tmp/deployments.json"));
    }

    #[test]
    fn candidate_list_deduplicates_primary_path() {
        let candidates = deployment_file_candidates(Path::new("data/state/deployments.json"));
        let data_state = candidates
            .iter()
            .filter(|candidate| candidate.as_path() == Path::new("data/state/deployments.json"))
            .count();
        assert_eq!(data_state, 1);
    }
}
