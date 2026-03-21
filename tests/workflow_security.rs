use std::fs;
use std::path::Path;

fn workflow_contents(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn ci_runs_dependency_vulnerability_audit() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content.contains("taiki-e/install-action@cargo-audit") {
        offenders.push("test.yml: missing cargo-audit installer step".to_string());
    }

    if !content.contains("cargo audit --ignore RUSTSEC-2023-0071") {
        offenders.push(
            "test.yml: missing cargo audit execution with the documented sqlx/rsa exception"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "workflow dependency-audit guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn release_platform_workflow_pins_host_fingerprints() {
    let content = workflow_contents(".github/workflows/release-platform.yml");
    let mut offenders = Vec::new();

    if content.matches("uses: appleboy/").count() != 2 {
        offenders.push(
            "release-platform.yml: expected exactly two appleboy steps (scp + ssh)".to_string(),
        );
    }

    if content.matches("fingerprint:").count() != 2 {
        offenders.push(
            "release-platform.yml: expected fingerprint pinning on both appleboy steps"
                .to_string(),
        );
    }

    if !content.contains("EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT") {
        offenders.push(
            "release-platform.yml: missing EC2 fingerprint secret wiring".to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "release-platform.yml fingerprint pinning check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn checked_in_platform_service_enforces_guardrails() {
    let content = workflow_contents("deployment/ployd.service");
    let required = [
        "Restart=always",
        "RestartSec=5",
        "StartLimitIntervalSec=300",
        "StartLimitBurst=5",
        "MemoryHigh=",
        "MemoryMax=",
        "OOMPolicy=kill",
        "EnvironmentFile=-/opt/ploy/.env",
        "ExecStart=/opt/ploy/bin/ployd",
    ];
    let mut offenders = Vec::new();

    if content.contains("StartLimitInterval=") {
        offenders.push(
            "deployment/ployd.service: still uses deprecated StartLimitInterval=".to_string(),
        );
    }

    for needle in required {
        if !content.contains(needle) {
            offenders.push(format!(
                "deployment/ployd.service: missing guardrail `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "checked-in ployd.service guardrail check failed:\n{}",
        offenders.join("\n")
    );
}
