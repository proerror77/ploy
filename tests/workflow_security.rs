use std::fs;
use std::path::Path;

fn workflow_contents(relative_path: &str) -> String {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(relative_path);
    fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

fn workflow_dispatch_input_count(content: &str) -> usize {
    let mut in_inputs = false;
    let mut base_indent = 0usize;
    let mut count = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        if trimmed == "inputs:" {
            in_inputs = true;
            base_indent = indent;
            continue;
        }
        if in_inputs {
            if indent <= base_indent {
                break;
            }
            if indent == base_indent + 2 && trimmed.ends_with(':') {
                count += 1;
            }
        }
    }

    count
}

#[test]
fn ci_runs_dependency_vulnerability_audit() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content.contains("name: Workflow lint") {
        offenders.push("test.yml: missing workflow lint job".to_string());
    }

    if !content.contains("actionlint/cmd/actionlint@v1.7.7") {
        offenders.push("test.yml: missing pinned actionlint installation".to_string());
    }

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
fn workflow_dispatch_inputs_stay_lintable() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workflow_dir = repo_root.join(".github/workflows");
    let mut offenders = Vec::new();

    for entry in fs::read_dir(&workflow_dir)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", workflow_dir.display()))
    {
        let path = entry.expect("workflow dir entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("yml") {
            continue;
        }
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        if !content.contains("workflow_dispatch:") {
            continue;
        }
        let input_count = workflow_dispatch_input_count(&content);
        if input_count > 10 {
            offenders.push(format!(
                "{}: workflow_dispatch has {input_count} inputs; move advanced knobs to options_json",
                path.strip_prefix(repo_root).unwrap_or(&path).display()
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "workflow_dispatch input-count guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn ack_images_use_immutable_sha_tags() {
    let build = workflow_contents(".github/workflows/build-push-acr.yml");
    let deploy = workflow_contents(".github/workflows/deploy-ack.yml");
    let mut offenders = Vec::new();

    if !build.contains("default: false") {
        offenders.push("build-push-acr.yml: image pushes should default to false".to_string());
    }
    if build.contains(":latest") {
        offenders.push("build-push-acr.yml: must not build or push latest tags".to_string());
    }
    if !build.contains("ACR image pushes must use git_ref=main") {
        offenders.push("build-push-acr.yml: missing main-only push provenance gate".to_string());
    }
    if deploy.contains("default: \"latest\"") || deploy.contains("SHA or 'latest'") {
        offenders.push("deploy-ack.yml: must not accept latest as a deploy default".to_string());
    }
    if !deploy.contains("environment: ack") {
        offenders.push("deploy-ack.yml: missing protected ack environment".to_string());
    }
    if !deploy.contains("^[0-9a-f]{40}$") {
        offenders.push("deploy-ack.yml: missing full SHA image-tag validation".to_string());
    }

    assert!(
        offenders.is_empty(),
        "ACK immutable image guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_workflows_do_not_transfer_runtime_binaries_between_jobs() {
    let optimize = workflow_contents(".github/workflows/optimize.yml");
    let backtest = workflow_contents(".github/workflows/backtest.yml");
    let mut offenders = Vec::new();

    for forbidden in [
        "Upload optimize runner",
        "Download optimize runner",
        "optimize-runner-${{ github.sha }}",
    ] {
        if optimize.contains(forbidden) {
            offenders.push(format!(
                "optimize.yml: fragile binary artifact pattern still contains `{forbidden}`"
            ));
        }
    }
    for forbidden in [
        "Upload run_backtest binary",
        "Download run_backtest binary",
        "run_backtest-${{ github.sha }}",
    ] {
        if backtest.contains(forbidden) {
            offenders.push(format!(
                "backtest.yml: fragile binary artifact pattern still contains `{forbidden}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "research workflow binary handoff guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn replay_dryrun_parity_reports_strict_readiness() {
    let script = workflow_contents("scripts/replay_dryrun_parity.py");
    let workflow = workflow_contents(".github/workflows/replay-dryrun-parity.yml");
    let mut offenders = Vec::new();

    if !script.contains("strict_parity_ready") {
        offenders
            .push("replay_dryrun_parity.py: missing strict parity readiness field".to_string());
    }
    if !script.contains("missing_strict_parity_fields") {
        offenders.push("replay_dryrun_parity.py: missing strict parity caveat".to_string());
    }
    if !workflow.contains("Replay/dry-run parity evidence") {
        offenders.push("replay-dryrun-parity.yml: missing issue evidence comment".to_string());
    }

    assert!(
        offenders.is_empty(),
        "replay/dry-run parity guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_issue_workflows_apply_decision_labels() {
    let helper = workflow_contents(".github/scripts/research-issue-labels.js");
    let mut offenders = Vec::new();

    for needle in [
        "applyResearchIssueLabels",
        "labelsForDecision",
        "decision:pending",
        "decision:fix-data",
        "decision:fix-runtime",
        "evidence:missing-metrics",
        "parity:blocked",
    ] {
        if !helper.contains(needle) {
            offenders.push(format!("research-issue-labels.js: missing `{needle}`"));
        }
    }

    for (workflow, evidence_label) in [
        (".github/workflows/backtest.yml", "evidence:backtest"),
        (
            ".github/workflows/replay-dryrun-parity.yml",
            "evidence:parity",
        ),
        (
            ".github/workflows/factor-review-v2.yml",
            "evidence:factor-review",
        ),
        (
            ".github/workflows/factor-walk-forward-v2.yml",
            "evidence:walk-forward",
        ),
        (".github/workflows/optimize.yml", "evidence:optimize"),
    ] {
        let content = workflow_contents(workflow);
        if !content.contains("research-issue-labels.js") {
            offenders.push(format!("{workflow}: missing shared research label helper"));
        }
        if !content.contains(evidence_label) {
            offenders.push(format!(
                "{workflow}: missing evidence label `{evidence_label}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "research issue label guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn strategy_research_runbook_stays_strategy_agnostic() {
    let runbook = workflow_contents("docs/runbooks/strategy-research-cicd.md");
    let research_template = workflow_contents(".github/ISSUE_TEMPLATE/strategy_research.yml");
    let implementation_template =
        workflow_contents(".github/ISSUE_TEMPLATE/strategy_implementation.yml");
    let backtest = workflow_contents(".github/workflows/backtest.yml");
    let optimize = workflow_contents(".github/workflows/optimize.yml");
    let mut offenders = Vec::new();

    for needle in [
        "Strategy-Agnostic Research and Runtime CI/CD Runbook",
        "Four-Layer Model",
        "Platform CI",
        "Research CI",
        "Runtime CD",
        "Promotion Gate",
        "PM5D is one current strategy profile, not the center of the CI/CD model",
        "strategy_family",
        "strategy_profile",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("strategy-research-cicd.md: missing `{needle}`"));
        }
    }

    for forbidden in [
        "PM5D factor diagnostics",
        "PM5D walk-forward diagnostics",
        "PM5D backtest",
        "For PM5D, one event",
    ] {
        if runbook.contains(forbidden) {
            offenders.push(format!(
                "strategy-research-cicd.md: workflow contract is still PM5D-centered via `{forbidden}`"
            ));
        }
    }

    if research_template.contains("PM5D") {
        offenders
            .push("strategy_research.yml: placeholders should be strategy-agnostic".to_string());
    }
    if !research_template.contains("id: strategy_family")
        || !research_template.contains("id: strategy_profile")
    {
        offenders.push("strategy_research.yml: missing strategy family/profile fields".to_string());
    }
    if !implementation_template.contains("id: strategy_family")
        || !implementation_template.contains("id: strategy_profile")
    {
        offenders.push(
            "strategy_implementation.yml: missing strategy family/profile fields".to_string(),
        );
    }
    if !backtest.contains("name: Strategy Backtest") || backtest.contains("## PM5D Backtest") {
        offenders.push("backtest.yml: workflow naming should be strategy-agnostic".to_string());
    }
    if !optimize.contains("name: Optimize strategy params") {
        offenders.push("optimize.yml: workflow naming should be strategy-agnostic".to_string());
    }

    assert!(
        offenders.is_empty(),
        "strategy-agnostic CI/CD guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn auto_review_uses_bounded_workspace_checks() {
    let content = workflow_contents(".github/workflows/auto-review.yml");
    let mut offenders = Vec::new();

    if content.contains("cargo fmt --all -- --check") {
        offenders.push(
            "auto-review.yml: cargo fmt --all also formats local path dependencies such as vendor SDKs"
                .to_string(),
        );
    }
    if content.contains("cargo clippy --all-targets --features rl") {
        offenders.push(
            "auto-review.yml: rl is not a feature on the default workspace member set".to_string(),
        );
    }
    if !content.contains("git diff --name-only --diff-filter=ACMRT")
        || !content.contains("':!vendor/**'")
        || !content.contains("xargs rustfmt --check")
    {
        offenders.push(
            "auto-review.yml: formatting should target PR Rust file changes only and exclude vendor path dependencies"
                .to_string(),
        );
    }
    if content.contains("cargo clippy") || content.contains("Swatinem/rust-cache") {
        offenders.push(
            "auto-review.yml: advisory review should not duplicate heavy compile/test matrix work"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "auto-review workflow guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn test_matrix_owns_research_heavy_feature_contract() {
    let content = workflow_contents(".github/workflows/test.yml");
    let mut offenders = Vec::new();

    if !content
        .contains("cargo check --locked -p ploy-research --features db,polars-export,ml,rl,strategy-runtime --lib")
    {
        offenders.push(
            "test.yml: missing ploy-research heavy feature contract check".to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "research heavy CI guard failed:\n{}",
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
            "release-platform.yml: expected fingerprint pinning on both appleboy steps".to_string(),
        );
    }

    if !content.contains("EC2_HOST_FINGERPRINT || secrets.AWS_EC2_HOST_FINGERPRINT") {
        offenders.push("release-platform.yml: missing EC2 fingerprint secret wiring".to_string());
    }

    assert!(
        offenders.is_empty(),
        "release-platform.yml fingerprint pinning check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn host_deploy_workflows_require_main_provenance_and_pinned_ssh() {
    let tango = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let trade = workflow_contents(".github/workflows/deploy-trade.yml");
    let mut offenders = Vec::new();

    for (name, content, environment, known_hosts_secret) in [
        (
            "deploy-tango-1-1.yml",
            &tango,
            "environment: tango-1-1",
            "TANGO_1_1_KNOWN_HOSTS",
        ),
        (
            "deploy-trade.yml",
            &trade,
            "environment: ploy-trade-1",
            "PLOY_TRADE_1_KNOWN_HOSTS",
        ),
    ] {
        if !content.contains(environment) {
            offenders.push(format!("{name}: missing protected deployment environment"));
        }
        if !content.contains("Validate deploy provenance") {
            offenders.push(format!("{name}: missing deploy provenance validation step"));
        }
        if !content.contains("must dispatch the workflow from main")
            || !content.contains("must use git_ref=main")
            || !content.contains("does not match origin/main")
        {
            offenders.push(format!(
                "{name}: deployment is not hard-gated to main provenance"
            ));
        }
        if content.contains("StrictHostKeyChecking no")
            || content.contains("UserKnownHostsFile /dev/null")
        {
            offenders.push(format!("{name}: disables SSH host-key verification"));
        }
        if !content.contains("StrictHostKeyChecking yes")
            || !content.contains("UserKnownHostsFile ~/.ssh/known_hosts")
            || !content.contains(known_hosts_secret)
        {
            offenders.push(format!(
                "{name}: missing pinned known_hosts SSH verification"
            ));
        }
    }

    if !trade.contains("default: false") {
        offenders
            .push("deploy-trade.yml: live trade deploy should default deploy=false".to_string());
    }

    assert!(
        offenders.is_empty(),
        "host deploy workflow hardening check failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn research_workflows_require_private_tango_db_endpoint() {
    let mut offenders = Vec::new();

    let backtest = workflow_contents(".github/workflows/backtest.yml");
    if !backtest.contains(
        "PLOY_RESEARCH_DATABASE_URL must target Tango-1-1 private VPC endpoint 172.16.0.204",
    ) {
        offenders.push("backtest.yml: missing private Tango DB endpoint guard".to_string());
    }
    if !backtest.contains(
        "urlparse(os.environ[\"PLOY_RESEARCH_DATABASE_URL\"]).hostname",
    ) {
        offenders.push("backtest.yml: must parse the research DB URL host before use".to_string());
    }

    let factor_review = workflow_contents(".github/workflows/factor-review-v2.yml");
    if !factor_review
        .contains("PLOY_DB_URL must target Tango-1-1 private VPC endpoint 172.16.0.204")
    {
        offenders.push("factor-review-v2.yml: missing private Tango DB endpoint guard".to_string());
    }
    if !factor_review.contains("urlparse(os.environ[\"PLOY_DB_URL\"]).hostname") {
        offenders.push(
            "factor-review-v2.yml: must parse the research DB URL host before use".to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "research workflow private DB guard failed:\n{}",
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
