use std::fs;
use std::path::Path;

fn repo_file(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn release_platform_workflow_builds_new_workspace_binaries() {
    let content = repo_file(".github/workflows/release-platform.yml");
    let mut offenders = Vec::new();

    for needle in [
        "cargo build --release --locked",
        "apps/ployd",
        "apps/ployctl",
        "bin/ployd",
        "bin/ployctl",
        "deployment/ployd.service",
        "scripts/install-platform-service.sh",
        "systemctl status ployd",
        "curl -fsS http://127.0.0.1:8081/health",
        "ployctl system status",
    ] {
        if !content.contains(needle) {
            offenders.push(format!(
                "release-platform.yml: missing `{needle}` in the new platform release path"
            ));
        }
    }

    if content.contains("target/release/ploy") {
        offenders.push("release-platform.yml: still references legacy target/release/ploy".to_string());
    }

    assert!(
        offenders.is_empty(),
        "platform release workflow guard failed:\n{}",
        offenders.join("\n")
    );
}
