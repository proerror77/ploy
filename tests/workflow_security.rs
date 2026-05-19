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
fn factor_evolve_daily_search_passes_snapshot_quote_age() {
    let workflow = workflow_contents(".github/workflows/factor-evolve-daily-research.yml");
    let mut offenders = Vec::new();

    for needle in [
        "max_quote_age_secs:",
        "MAX_QUOTE_AGE_SECS: ${{ github.event.inputs.max_quote_age_secs }}",
        "\"max_quote_age_secs\": max_quote_age_secs",
        "-f options_json=\"$(cat artifacts/factor-evolve-daily/hosted-options.json)\"",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("missing daily workflow quote-age handoff: {needle}"));
        }
    }

    assert!(
        offenders.is_empty(),
        "FactorEvolve daily search workflow guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn event_ml_rolling_evidence_has_hosted_artifact_lane() {
    let workflow = workflow_contents(".github/workflows/event-ml-rolling-evidence.yml");
    let runbook = workflow_contents("docs/runbooks/event-ml-automl-workflow.md");
    let mut offenders = Vec::new();

    for needle in [
        "source_dataset_run_id:",
        "actions: read",
        "Generate event ML rolling evidence from artifact on GitHub-hosted runner",
        "runs-on: ubuntu-latest",
        "github.event.inputs.source_dataset_run_id != ''",
        "scripts/download_github_artifact.py",
        "event-ml-rolling-datasets-${SOURCE_DATASET_RUN_ID}",
        "--runtime-score",
        "--replay-parity-ready",
        "create_handoff_issue",
        "create_config_pr",
        "model_artifact_path",
        "Skip config PR on legacy DB branch",
        "Create config PR from ready Event ML handoff",
        "issues: write",
        "pull-requests: write",
        "Event ML handoff status is ${status}; no dry-run issue will be created.",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("event-ml-rolling-evidence.yml: missing `{needle}`"));
        }
    }

    if !workflow.contains("github.event.inputs.source_dataset_run_id == ''")
        || !workflow.contains("Generate event ML rolling evidence from DB on ploy-ci-1")
    {
        offenders.push(
            "event-ml-rolling-evidence.yml: legacy DB export must be explicitly isolated"
                .to_string(),
        );
    }

    for needle in [
        "Prefer the hosted artifact path",
        "source_dataset_artifact_name",
        "runtime_score",
        "replay_parity_ready",
        "create_handoff_issue",
        "create_config_pr",
        "model_artifact_path",
        "workflow stays within GitHub's 10-input dispatch limit",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("event-ml-automl-workflow.md: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "event ML hosted artifact lane guard failed:\n{}",
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
fn recorded_replay_parity_supports_auto_window() {
    let workflow = workflow_contents(".github/workflows/recorded-replay-parity.yml");
    let runbook = workflow_contents("docs/runbooks/strategy-research-cicd.md");
    let mut offenders = Vec::new();

    for needle in [
        "approval_environment:",
        "default: \"tango-1-1-build-only\"",
        "Build replay runner from workflow ref",
        "--features new-ploy-runner/full",
        "-p new-ploy-runner",
        "target/${PLATFORM_TARGET}/release/new-ploy-runner",
        "tango-1-1:\"${REMOTE_DIR}/ploy-runner\"",
        "timeout 600 \"${remote_dir}/ploy-runner\" run",
        "skip_settlement_exits:",
        "Skip settlement exits for entry-only dry-run parity",
        "SKIP_SETTLEMENT_EXITS",
        "skip_settlement_exits must be true or false",
        "environment: ${{ inputs.approval_environment }}",
        "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
        "pm5d-threelayer-settlement-probability-btc-eth.ndjson",
        "TANGO_SSH_KEY",
        "ALIYUN_ECS_SSH_KEY",
        "No SSH key secret found for tango-1-1",
        "ssh-keygen -y -f ~/.ssh/tango_1_1_key",
        "default: \"auto\"",
        "resolve_recorded_replay_window.py",
        "--recording \"${recording_path}\"",
        "--dryrun-json \"${dryrun_report}\"",
        "StrictHostKeyChecking yes",
        "UserKnownHostsFile ~/.ssh/known_hosts",
        "TANGO_1_1_KNOWN_HOSTS",
        "resolved-window.json",
        "resolved-window.env",
        "lines.append(f\"skip_settlement_exits = {skip_settlement_exits}\")",
        "extract_official_settlement_evidence.py",
        "official-settlement-token-ids.json",
        "official-settlement-token-ids.tsv",
        "official-settlement-db-rows.json",
        "official-settlement-report.json",
        "CREATE TEMP TABLE replay_token_ids",
        "\\copy replay_token_ids(token_id)",
        "\\o ${official_db_rows_json}",
        "FROM pm_token_settlements s",
        "JOIN replay_token_ids t ON t.token_id = s.token_id",
        "--db-settlements-json \"${official_db_rows_json}\"",
        "RESOLVED_SINCE",
        "RESOLVED_UNTIL",
        "Requested window",
        "Resolved window",
        "Auto-window closed rows",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("recorded-replay-parity.yml: missing `{needle}`"));
        }
    }
    for needle in [
        "builds `new-ploy-runner` on\n",
        "does not deploy artifacts, restart\nservices, replace `/opt/ploy/bin/ploy-runner`, or enable live orders",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!(
                "strategy-research-cicd.md: missing recorded replay note `{needle}`"
            ));
        }
    }
    if workflow.contains("extract_dryrun_settlement_evidence.py") {
        offenders.push(
            "recorded-replay-parity.yml: dry-run settlement extraction must not feed official replay enrichment"
                .to_string(),
        );
    }
    if workflow.contains("FROM strategy_runtime_event_track_record") {
        offenders.push(
            "recorded-replay-parity.yml: official replay enrichment must not source settlement labels from track-record rows"
                .to_string(),
        );
    }
    if workflow.contains("-v token_ids_json=") || workflow.contains("jsonb_array_elements_text(:'token_ids_json'") {
        offenders.push(
            "recorded-replay-parity.yml: token ids must not be passed as one large psql argv value"
                .to_string(),
        );
    }

    for needle in [
        "defaults `since=auto` and `until=auto`",
        "records the resolved window",
        "timestamps remain supported",
        "`approval_environment` input therefore defaults to",
        "`tango-1-1-build-only`",
        "`TANGO_SSH_KEY` / `ALIYUN_ECS_SSH_KEY`",
    ] {
        if !runbook.contains(needle) {
            offenders.push(format!("strategy-research-cicd.md: missing `{needle}`"));
        }
    }

    assert!(
        offenders.is_empty(),
        "recorded replay parity auto-window guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_uploads_alpha_chain_summary() {
    let workflow = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "Summarize alpha search chain evidence",
        "scripts/summarize_alpha_search_chain.py",
        "artifacts/factor-walk-forward-v2-upload",
        "alpha-search-chain/summary.json",
        "alpha-search-chain/summary.md",
        "Upload report artifact",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    let summary_index = workflow
        .find("Summarize alpha search chain evidence")
        .unwrap_or(usize::MAX);
    let upload_index = workflow.find("Upload report artifact").unwrap_or(0);
    if summary_index > upload_index {
        offenders.push(
            "factor-walk-forward-v2-hosted-artifact.yml: summary must be generated before upload"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "hosted alpha chain summary artifact guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn factor_walk_forward_wires_alpha_prior_and_state_inputs() {
    let hosted = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let self_hosted = workflow_contents(".github/workflows/factor-walk-forward-v2.yml");
    let mut offenders = Vec::new();

    for (name, workflow) in [
        ("factor-walk-forward-v2-hosted-artifact.yml", hosted.as_str()),
        ("factor-walk-forward-v2.yml", self_hosted.as_str()),
    ] {
        for needle in [
            "alpha_search_llm_prior_json",
            "alpha_search_state_json",
            "require_deribit",
            "train_window_hours",
            "test_window_hours",
            "step_hours",
            "mcts-state.json",
            "--alpha-search-state-json",
            "--alpha-search-llm-prior-json",
            "--require-deribit",
        ] {
            if !workflow.contains(needle) {
                offenders.push(format!("{name}: missing `{needle}`"));
            }
        }
    }

    let route_allowlist = self_hosted
        .split("allowed = {")
        .nth(1)
        .and_then(|tail| tail.split("raw = os.environ").next())
        .unwrap_or("");
    for needle in [
        "alpha_search_plan_run_id",
        "alpha_search_plan_artifact_name",
        "alpha_search_llm_prior_json",
        "alpha_search_state_json",
        "require_deribit",
        "train_window_hours",
        "test_window_hours",
        "step_hours",
    ] {
        if !route_allowlist.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2.yml: hosted route allowlist missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "alpha-search prior/state workflow wiring guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn hosted_factor_walk_forward_splits_replay_parity_artifact_suffix() {
    let workflow = workflow_contents(".github/workflows/factor-walk-forward-v2-hosted-artifact.yml");
    let mut offenders = Vec::new();

    for needle in [
        "replay_parity_run_id",
        "replay_parity_artifact_name",
        "split(\":\", 1)",
        "replay_parity_run_id must be <run-id>:<artifact-name>",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!(
                "factor-walk-forward-v2-hosted-artifact.yml: missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "hosted factor walk-forward replay parity artifact split guard failed:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn tango_deploy_keeps_pm5d_live_paused() {
    let workflow = workflow_contents(".github/workflows/deploy-tango-1-1.yml");
    let cloud_assist = workflow_contents("scripts/ci/deploy_tango_cloud_assist.py");
    let mut offenders = Vec::new();

    for needle in [
        "environment: ${{ inputs.deploy && 'tango-1-1' || 'tango-1-1-build-only' }}",
        "Verify live deployment remains paused in bundle",
        "pm5d.threelayer.live.json",
        "desired_state=paused",
        "deployments inspect pm5d.threelayer.live",
        "deployments inspect pm5d.threelayer.live 2>&1",
        "awk '\\$1 == \"pm5d.threelayer.live\"",
        "desired=Paused",
        "observed=Paused",
    ] {
        if !workflow.contains(needle) {
            offenders.push(format!("deploy-tango-1-1.yml: missing `{needle}`"));
        }
    }

    for needle in [
        "require_pm5d_live_paused",
        "deployments inspect pm5d.threelayer.live",
        "deployments inspect pm5d.threelayer.live 2>&1",
        "awk '$1 == \"pm5d.threelayer.live\"",
        "desired=Paused",
        "observed=Paused",
    ] {
        if !cloud_assist.contains(needle) {
            offenders.push(format!(
                "deploy_tango_cloud_assist.py: missing `{needle}`"
            ));
        }
    }

    assert!(
        offenders.is_empty(),
        "tango deploy live-paused guard failed:\n{}",
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
    if !backtest.contains("urlparse(os.environ[\"PLOY_RESEARCH_DATABASE_URL\"]).hostname") {
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

    if content.contains("download-artifact")
        || content.contains("optimize_backtest-${{ github.sha }}")
    {
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
