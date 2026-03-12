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

    if !content.contains("cargo audit") {
        offenders.push("test.yml: missing cargo audit execution step".to_string());
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
