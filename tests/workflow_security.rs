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
fn ssh_workflows_require_host_key_verification() {
    let workflows = [
        ".github/workflows/deploy-aws-jp.yml",
        ".github/workflows/get-logs.yml",
        ".github/workflows/stop-trading.yml",
    ];

    let mut offenders = Vec::new();
    for workflow in workflows {
        let content = workflow_contents(workflow);

        if content.contains("StrictHostKeyChecking=no") {
            offenders.push(format!(
                "{workflow}: disables SSH host key verification with StrictHostKeyChecking=no"
            ));
        }

        if !content.contains("AWS_EC2_KNOWN_HOSTS") {
            offenders.push(format!(
                "{workflow}: missing pinned AWS_EC2_KNOWN_HOSTS secret wiring"
            ));
        }

        if !content.contains("StrictHostKeyChecking=yes")
            || !content.contains("UserKnownHostsFile=\"$HOME/.ssh/known_hosts\"")
        {
            offenders.push(format!(
                "{workflow}: missing strict known_hosts-backed SSH enforcement"
            ));
        }

        if !content.contains("ssh-keygen -F \"$HOST\" -f ~/.ssh/known_hosts") {
            offenders.push(format!(
                "{workflow}: missing explicit host entry validation against pinned known_hosts"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow ssh hardening guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn release_workflows_enforce_systemd_guardrails() {
    let workflows = [
        (
            ".github/workflows/release-aliyun.yml",
            vec![
                "StartLimitIntervalSec=300",
                "StartLimitBurst=5",
                "Restart=always",
                "RestartSec=${PLOY_SYSTEMD_RESTART_SEC}",
                "MemoryHigh=${PLOY_SYSTEMD_MEMORY_HIGH}",
                "MemoryMax=${PLOY_SYSTEMD_MEMORY_MAX}",
                "OOMPolicy=kill",
            ],
        ),
        (
            ".github/workflows/deploy-prebuilt.yml",
            vec![
                "StartLimitIntervalSec=300",
                "StartLimitBurst=5",
                "Restart=always",
                "RestartSec=5",
                "MemoryHigh=1280M",
                "MemoryMax=1536M",
                "OOMPolicy=kill",
            ],
        ),
        (
            ".github/workflows/deploy-tango21.yml",
            vec![
                "StartLimitIntervalSec=300",
                "StartLimitBurst=5",
                "Restart=always",
                "RestartSec=5",
                "MemoryHigh=1280M",
                "MemoryMax=1536M",
                "OOMPolicy=kill",
            ],
        ),
    ];
    let mut offenders = Vec::new();

    for (workflow, needles) in workflows {
        let content = workflow_contents(workflow);

        for needle in needles {
            if !content.contains(needle) {
                offenders.push(format!("{workflow}: missing systemd guardrail `{needle}`"));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow systemd guardrail check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn appleboy_workflows_pin_host_fingerprints() {
    let workflows = [
        (
            ".github/workflows/deploy.yml",
            5,
            "EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT",
        ),
        (
            ".github/workflows/release.yml",
            2,
            "EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT",
        ),
        (
            ".github/workflows/release-staging.yml",
            2,
            "STAGING_EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT",
        ),
        (
            ".github/workflows/release-aliyun.yml",
            2,
            "ALIYUN_ECS_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT",
        ),
        (
            ".github/workflows/rollback.yml",
            1,
            "EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT",
        ),
    ];
    let mut offenders = Vec::new();

    for (workflow, expected_appleboy_steps, fingerprint_secret) in workflows {
        let content = workflow_contents(workflow);
        let appleboy_steps = content.matches("uses: appleboy/").count();
        let fingerprint_count = content.matches("fingerprint:").count();

        if appleboy_steps != expected_appleboy_steps {
            offenders.push(format!(
                "{workflow}: expected {expected_appleboy_steps} appleboy steps, found {appleboy_steps}"
            ));
        }

        if fingerprint_count != expected_appleboy_steps {
            offenders.push(format!(
                "{workflow}: expected {expected_appleboy_steps} fingerprint pins, found {fingerprint_count}"
            ));
        }

        if !content.contains(fingerprint_secret) {
            offenders.push(format!(
                "{workflow}: missing expected fingerprint secret wiring `{fingerprint_secret}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow appleboy fingerprint check failed:\n{}",
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
            offenders.push(format!("deployment/ployd.service: missing guardrail `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "checked-in ployd.service guardrail check failed:\n{}",
        offenders.join("\n")
    );
}
