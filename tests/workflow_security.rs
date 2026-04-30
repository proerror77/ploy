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
fn rust_toolchain_actions_pin_required_toolchain_input() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflows_dir = repo_root.join(".github/workflows");
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&workflows_dir).expect("failed to read workflow directory") {
        let path = entry.expect("failed to read workflow entry").path();
        if !matches!(path.extension().and_then(|ext| ext.to_str()), Some("yml" | "yaml")) {
            continue;
        }
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let lines: Vec<_> = content.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains("uses: dtolnay/rust-toolchain@master") {
                continue;
            }
            let window = lines
                .iter()
                .skip(idx + 1)
                .take(6)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if !window.contains("toolchain: stable") {
                offenders.push(format!(
                    "{}:{}: dtolnay/rust-toolchain@master must set `toolchain: stable`",
                    path.strip_prefix(repo_root).unwrap_or(&path).display(),
                    idx + 1
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "rust toolchain workflow guard failed:\n{}",
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
fn ack_acr_workflows_require_immutable_image_tags() {
    let build = workflow_contents(".github/workflows/build-push-acr.yml");
    let deploy = workflow_contents(".github/workflows/deploy-ack.yml");
    let mut offenders = Vec::new();

    if !build.contains("default: false") {
        offenders.push("build-push-acr.yml: push_images must default to false".to_string());
    }
    if build.contains(":latest") {
        offenders.push("build-push-acr.yml: must not build or push mutable :latest tags".to_string());
    }
    if !build.contains("BUILD_SHA=$(git rev-parse HEAD)") {
        offenders.push("build-push-acr.yml: must tag images with the checked-out commit".to_string());
    }
    if !build.contains("ACR image pushes must use git_ref=main") {
        offenders.push("build-push-acr.yml: image pushes must be gated to git_ref=main".to_string());
    }

    if deploy.contains("default: \"latest\"") || deploy.contains("image_tag: latest") {
        offenders.push("deploy-ack.yml: must not default to or allow latest image tags".to_string());
    }
    if !deploy.contains("^[0-9a-f]{40}$") {
        offenders.push("deploy-ack.yml: must require full immutable SHA image tags".to_string());
    }
    if !deploy.contains("provenance_ref") || !deploy.contains("compare/${image_tag}...${provenance_ref}") {
        offenders.push("deploy-ack.yml: must validate image SHA provenance against main or release refs".to_string());
    }
    if !deploy.contains("environment: ack") {
        offenders.push("deploy-ack.yml: must use an environment-scoped deployment".to_string());
    }

    assert!(
        offenders.is_empty(),
        "ACK/ACR immutable-image guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn mutating_deploy_workflows_enforce_main_only() {
    let workflows = [
        (
            ".github/workflows/deploy-tango-1-1.yml",
            "Deployments that mutate tango-1-1 must use git_ref=main",
        ),
        (
            ".github/workflows/deploy-trade.yml",
            "Deployments that mutate ploy-trade-1 must use git_ref=main",
        ),
        (
            ".github/workflows/release-platform.yml",
            "Platform deployments must use git_ref=main",
        ),
    ];
    let mut offenders = Vec::new();

    for (path, message) in workflows {
        let content = workflow_contents(path);
        if !content.contains("if: ${{ inputs.deploy }}") {
            offenders.push(format!("{path}: main-only guard must apply when deploy=true"));
        }
        if !content.contains(message) {
            offenders.push(format!("{path}: missing main-only deploy guard message"));
        }
    }

    assert!(
        offenders.is_empty(),
        "main-only deploy guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn strategy_research_evidence_comments_are_decision_grade() {
    let backtest = workflow_contents(".github/workflows/backtest.yml");
    let parity_workflow = workflow_contents(".github/workflows/replay-dryrun-parity.yml");
    let parity_script = workflow_contents("scripts/replay_dryrun_parity.py");
    let mut offenders = Vec::new();

    if !backtest.contains("missing_headline_metrics") {
        offenders.push("backtest.yml: must flag empty headline metrics".to_string());
    }
    if !backtest.contains("missing_evaluation_artifact") {
        offenders.push("backtest.yml: must flag missing evaluation artifacts".to_string());
    }
    if !backtest.contains("fix-workflow-or-data-source") {
        offenders.push("backtest.yml: failed data-source runs must not look pending-successful".to_string());
    }
    if !parity_workflow.contains("Strict parity ready") {
        offenders.push("replay-dryrun-parity.yml: issue evidence must show strict parity readiness".to_string());
    }
    if !parity_workflow.contains("Missing strict fields") {
        offenders.push("replay-dryrun-parity.yml: issue evidence must list missing strict fields".to_string());
    }
    if !parity_script.contains("STRICT_FIELD_ALIASES") {
        offenders.push("replay_dryrun_parity.py: must normalize known event-field aliases".to_string());
    }
    if !parity_script.contains("strict_parity_ready") {
        offenders.push("replay_dryrun_parity.py: must emit strict parity readiness".to_string());
    }

    assert!(
        offenders.is_empty(),
        "strategy research evidence guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn optimize_workflow_builds_and_runs_in_one_job() {
    let content = workflow_contents(".github/workflows/optimize.yml");
    let mut offenders = Vec::new();

    if content.contains("download-artifact") || content.contains("optimize_backtest-${{ github.sha }}") {
        offenders.push(
            "optimize.yml: must not pass optimize_backtest through a binary artifact".to_string(),
        );
    }
    if content.contains("Swatinem/rust-cache") {
        offenders.push(
            "optimize.yml: must not use Swatinem/rust-cache after cache post-step hangs"
                .to_string(),
        );
    }
    if !content.contains("name: Build and run optimize on ploy-ci-1") {
        offenders.push("optimize.yml: must use the single build-and-run job".to_string());
    }

    assert!(
        offenders.is_empty(),
        "optimize workflow single-job guard failed:\n{}",
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
