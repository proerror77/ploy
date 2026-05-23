//! Persist Research OS artifacts into durable PostgreSQL trace tables.
//!
//! This writer is intentionally conservative: alpha-search artifacts can record
//! factor attribution and blocker state, but they do not promote a strategy to
//! dry-run or live without replay/runtime parity evidence.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ploy_research::ResearchSnapshotManifest;
use ploy_research::research_os::trace::trace_hash;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

const WRITER_AGENT: &str = "persist_research_trace";
const EVALUATOR_VERSION: &str = "persist_research_trace_v1";
const EVIDENCE_STAGE: &str = "factor_attribution";
const EVALUATION_KIND: &str = "alpha_search_preview";
const FACTOR_HORIZON: &str = "5m";

#[derive(Debug, Clone)]
struct TracePlan {
    run_id: String,
    snapshot_dir: PathBuf,
    alpha_search_dir: Option<PathBuf>,
    registry_json: Option<PathBuf>,
    promotion_json: Option<PathBuf>,
    handoff_json: Option<PathBuf>,
    candidate_replay_jsons: Vec<PathBuf>,
    dry_run: bool,
}

#[derive(Debug, Clone)]
struct DatasetSnapshotRow {
    data_snapshot_id: String,
    snapshot_hash: Option<String>,
    schema_version: String,
    source_kind: String,
    dataset_start_ts: DateTime<Utc>,
    dataset_end_ts: DateTime<Utc>,
    symbols: Vec<String>,
    row_counts_json: Value,
    source_surfaces_json: Value,
    input_artifacts_json: Value,
    sampling_json: Value,
    manifest_json: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FactorDecision {
    registry_status: String,
    promotion_decision: String,
    promotion_status: String,
    passed_gate: bool,
}

#[derive(Debug, Clone)]
struct FactorPreviewRow {
    factor_name: String,
    target: String,
    dsl_hash: String,
    ast_json: Value,
    runtime_contract: Value,
    factor_family: String,
    horizon: String,
    registry_status: String,
    promotion_decision: String,
    promotion_status: String,
    passed_gate: bool,
    blockers: Value,
    metrics: Value,
    raw: Value,
}

#[derive(Debug, Clone)]
struct CandidateReplayTapeRow {
    candidate_replay_id: String,
    run_id: String,
    source_workflow: String,
    workflow_run_id: Option<String>,
    workflow_run_url: Option<String>,
    artifact_name: Option<String>,
    artifact_sha256: String,
    artifact_json: Value,
    basis: String,
    evidence_stage: String,
    deployment_id: Option<String>,
    strategy_profile: String,
    runtime_score: String,
    data_snapshot_id: String,
    dsl_hash: Option<String>,
    factor_name: Option<String>,
    target: Option<String>,
    horizon: Option<String>,
    recording_path: Option<String>,
    recording_sha256: Option<String>,
    config_path: Option<String>,
    config_sha256: Option<String>,
    runner_source: Option<String>,
    runner_git_sha: Option<String>,
    decision_contract_json: Value,
    acceptance_criteria_json: Value,
    metrics_json: Value,
    blocking_risk_flags_json: Value,
    promotion_ready: bool,
    promotion_decision: String,
    artifact_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ArtifactTrace {
    event_type: String,
    artifact_path: PathBuf,
    output_json: Value,
}

impl ArtifactTrace {
    fn evidence_stage(&self) -> &'static str {
        match self.event_type.as_str() {
            "factor_registry_preview" => EVIDENCE_STAGE,
            "alpha_search_tree_trace" | "alpha_search_feedback" => "diagnostic",
            "promotion_registry" | "autofactor_promotion" | "strategy_handoff" => "walk_forward",
            _ => "diagnostic",
        }
    }

    fn promotion_decision(&self) -> Option<&str> {
        self.output_json.get("decision").and_then(Value::as_str)
    }
}

impl FactorPreviewRow {
    fn dsl_source(&self) -> String {
        self.ast_json.to_string()
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|window| window[0] == flag)
        .map(|window| window[1].clone())
        .collect()
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn usage() -> &'static str {
    "usage: persist_research_trace --run-id <id> --snapshot-dir <dir> [--db-url <url>|DATABASE_URL] [--alpha-search-dir <dir>] [--registry-json <path>] [--promotion-json <path>] [--handoff-json <path>] [--candidate-replay-json <path>...] [--dry-run]"
}

fn parse_args(args: &[String]) -> Result<TracePlan> {
    let run_id = flag_value(args, "--run-id").context("--run-id required")?;
    let snapshot_dir =
        PathBuf::from(flag_value(args, "--snapshot-dir").context("--snapshot-dir required")?);
    Ok(TracePlan {
        run_id,
        snapshot_dir,
        alpha_search_dir: flag_value(args, "--alpha-search-dir").map(PathBuf::from),
        registry_json: flag_value(args, "--registry-json").map(PathBuf::from),
        promotion_json: flag_value(args, "--promotion-json").map(PathBuf::from),
        handoff_json: flag_value(args, "--handoff-json").map(PathBuf::from),
        candidate_replay_jsons: flag_values(args, "--candidate-replay-json")
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        dry_run: flag_present(args, "--dry-run"),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let plan = parse_args(&args).with_context(|| usage())?;
    let manifest = read_snapshot_manifest(&plan.snapshot_dir)?;
    let dataset = dataset_row_from_manifest(&plan.run_id, &manifest)?;
    let traces = collect_artifact_traces(&plan)?;
    let factor_rows = collect_factor_preview_rows(&plan)?;
    let candidate_replay_rows = collect_candidate_replay_rows(&plan, &dataset)?;

    eprintln!(
        "persist_research_trace: run_id={} snapshot={} factors={} candidate_replays={} traces={} dry_run={}",
        plan.run_id,
        dataset.data_snapshot_id,
        factor_rows.len(),
        candidate_replay_rows.len(),
        traces.len() + candidate_replay_rows.len() + 1,
        plan.dry_run
    );

    if plan.dry_run {
        return Ok(());
    }

    let db_url = flag_value(&args, "--db-url")
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .context("--db-url or DATABASE_URL required unless --dry-run")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await?;

    persist_dataset_snapshot(&pool, &dataset).await?;
    for factor in &factor_rows {
        let factor_id = upsert_factor_registry(&pool, factor).await?;
        upsert_factor_evaluation(&pool, &factor_id, &plan.run_id, &dataset, factor).await?;
    }
    for replay in &candidate_replay_rows {
        upsert_candidate_replay_tape(&pool, replay).await?;
        upsert_candidate_replay_evaluation(&pool, replay).await?;
    }

    append_experiment_trace(
        &pool,
        &plan.run_id,
        Some(&dataset.data_snapshot_id),
        None,
        None,
        "research_snapshot_manifest",
        "research_snapshot_manifest",
        "diagnostic",
        None,
        &plan.snapshot_dir.join("manifest.json"),
        &dataset.manifest_json,
    )
    .await?;
    for trace in &traces {
        append_experiment_trace(
            &pool,
            &plan.run_id,
            Some(&dataset.data_snapshot_id),
            None,
            None,
            &trace.event_type,
            &trace.event_type,
            trace.evidence_stage(),
            trace.promotion_decision(),
            &trace.artifact_path,
            &trace.output_json,
        )
        .await?;
    }
    for replay in &candidate_replay_rows {
        append_experiment_trace(
            &pool,
            &plan.run_id,
            Some(&dataset.data_snapshot_id),
            replay.dsl_hash.as_deref(),
            Some(&replay.candidate_replay_id),
            "candidate_replay_tape",
            "candidate_replay_tape",
            &replay.evidence_stage,
            Some(&replay.promotion_decision),
            &replay.artifact_path,
            &replay.artifact_json,
        )
        .await?;
    }

    eprintln!("persist_research_trace: persisted");
    Ok(())
}

fn read_snapshot_manifest(snapshot_dir: &Path) -> Result<ResearchSnapshotManifest> {
    read_json(&snapshot_dir.join("manifest.json")).with_context(|| {
        format!(
            "read research snapshot manifest from {}",
            snapshot_dir.display()
        )
    })
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
}

fn dataset_row_from_manifest(
    run_id: &str,
    manifest: &ResearchSnapshotManifest,
) -> Result<DatasetSnapshotRow> {
    let manifest_json = serde_json::to_value(manifest)?;
    let data_snapshot_id = manifest
        .snapshot_hash
        .clone()
        .filter(|hash| !hash.is_empty())
        .unwrap_or_else(|| format!("run:{run_id}"));
    Ok(DatasetSnapshotRow {
        data_snapshot_id,
        snapshot_hash: manifest.snapshot_hash.clone(),
        schema_version: manifest.schema_version.clone(),
        source_kind: manifest.source_kind.clone(),
        dataset_start_ts: manifest.start,
        dataset_end_ts: manifest.end,
        symbols: manifest.symbols.clone(),
        row_counts_json: serde_json::to_value(&manifest.row_counts)?,
        source_surfaces_json: serde_json::to_value(&manifest.source_surfaces)?,
        input_artifacts_json: serde_json::to_value(&manifest.input_artifacts)?,
        sampling_json: json!({
            "lob_sample_secs": manifest.lob_sample_secs,
            "pm_book_sample_secs": manifest.pm_book_sample_secs,
            "observation_sample_secs": manifest.observation_sample_secs,
            "max_quote_age_secs": manifest.max_quote_age_secs,
            "stake_usd": manifest.stake_usd,
            "require_official_settlement": manifest.require_official_settlement,
            "immutable_input": manifest.immutable_input,
            "data_requirements": manifest.data_requirements,
            "data_audit_status": manifest.data_audit_status,
            "data_audit_report": manifest.data_audit_report,
        }),
        manifest_json,
    })
}

fn collect_artifact_traces(plan: &TracePlan) -> Result<Vec<ArtifactTrace>> {
    let mut traces = Vec::new();
    if let Some(alpha_dir) = &plan.alpha_search_dir {
        for path in find_named_files(alpha_dir, "tree-trace.json")? {
            traces.push(read_artifact_trace("alpha_search_tree_trace", path)?);
        }
        for path in find_named_files(alpha_dir, "search-feedback.json")? {
            traces.push(read_artifact_trace("alpha_search_feedback", path)?);
        }
        for path in find_named_files(alpha_dir, "factor-registry-preview.json")? {
            traces.push(read_artifact_trace("factor_registry_preview", path)?);
        }
    }
    if let Some(path) = &plan.registry_json {
        traces.push(read_artifact_trace("promotion_registry", path.clone())?);
    }
    if let Some(path) = &plan.promotion_json {
        traces.push(read_artifact_trace("autofactor_promotion", path.clone())?);
    }
    if let Some(path) = &plan.handoff_json {
        traces.push(read_artifact_trace("strategy_handoff", path.clone())?);
    }
    traces.sort_by(|left, right| left.artifact_path.cmp(&right.artifact_path));
    Ok(traces)
}

fn read_artifact_trace(event_type: &str, artifact_path: PathBuf) -> Result<ArtifactTrace> {
    Ok(ArtifactTrace {
        event_type: event_type.to_string(),
        output_json: read_json(&artifact_path)?,
        artifact_path,
    })
}

fn collect_factor_preview_rows(plan: &TracePlan) -> Result<Vec<FactorPreviewRow>> {
    let mut rows = Vec::new();
    if let Some(alpha_dir) = &plan.alpha_search_dir {
        for path in find_named_files(alpha_dir, "factor-registry-preview.json")? {
            let preview: Value = read_json(&path)?;
            let target = string_field(&preview, "target")
                .or_else(|| target_from_preview_path(&path))
                .unwrap_or_else(|| "unknown".to_string());
            let horizon = string_field(&preview, "horizon")
                .unwrap_or_else(|| default_horizon_for_target(&target));
            let factors = preview_factors(&preview)
                .with_context(|| format!("{} missing factors array", path.display()))?;
            for factor in factors {
                rows.push(factor_preview_row(factor, &target, &horizon, &path)?);
            }
        }
    }
    rows.sort_by(|left, right| left.dsl_hash.cmp(&right.dsl_hash));
    Ok(rows)
}

fn preview_factors(preview: &Value) -> Option<&Vec<Value>> {
    if let Some(factors) = preview.get("factors").and_then(Value::as_array) {
        return Some(factors);
    }
    preview.as_array()
}

fn target_from_preview_path(path: &Path) -> Option<String> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|item| item.to_str())
        .filter(|item| !item.is_empty())
        .map(ToOwned::to_owned)
}

fn default_horizon_for_target(target: &str) -> String {
    if target.contains("10s") {
        "10s".to_string()
    } else if target.contains("30s") {
        "30s".to_string()
    } else {
        FACTOR_HORIZON.to_string()
    }
}

fn collect_candidate_replay_rows(
    plan: &TracePlan,
    dataset: &DatasetSnapshotRow,
) -> Result<Vec<CandidateReplayTapeRow>> {
    let mut rows = Vec::new();
    for path in &plan.candidate_replay_jsons {
        rows.push(candidate_replay_row(plan, dataset, path)?);
    }
    rows.sort_by(|left, right| left.candidate_replay_id.cmp(&right.candidate_replay_id));
    Ok(rows)
}

fn candidate_replay_row(
    plan: &TracePlan,
    dataset: &DatasetSnapshotRow,
    path: &Path,
) -> Result<CandidateReplayTapeRow> {
    let artifact_json: Value = read_json(path)?;
    let artifact_sha256 = file_sha256(path)?;
    let basis = string_field(&artifact_json, "basis")
        .with_context(|| format!("{} missing basis", path.display()))?;
    let evidence_stage = string_field(&artifact_json, "evidence_stage").unwrap_or_else(|| {
        if basis == "factor_walk_forward_top_bucket_aggregate" {
            "diagnostic".to_string()
        } else {
            "executable_replay".to_string()
        }
    });
    let candidate_replay_id = string_field(&artifact_json, "candidate_replay_id")
        .unwrap_or_else(|| format!("candidate_replay:{}", &artifact_sha256[..32]));
    let source_factor = artifact_json
        .get("source_factor")
        .and_then(Value::as_object);
    let dsl_hash = source_factor
        .and_then(|item| item.get("dsl_hash"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let factor_name = source_factor
        .and_then(|item| item.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| factor_name_from_runtime_score(&artifact_json));
    let target = source_factor
        .and_then(|item| item.get("target"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let horizon = source_factor
        .and_then(|item| item.get("horizon"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| Some(FACTOR_HORIZON.to_string()));
    let promotion_ready = artifact_json
        .get("promotion_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let promotion_decision =
        string_field(&artifact_json, "promotion_decision").unwrap_or_else(|| {
            if promotion_ready {
                "promote_to_runtime".to_string()
            } else {
                "blocked".to_string()
            }
        });

    Ok(CandidateReplayTapeRow {
        candidate_replay_id,
        run_id: plan.run_id.clone(),
        source_workflow: string_field(&artifact_json, "source_workflow")
            .unwrap_or_else(|| "unknown".to_string()),
        workflow_run_id: string_field(&artifact_json, "workflow_run_id"),
        workflow_run_url: string_field(&artifact_json, "workflow_run_url"),
        artifact_name: string_field(&artifact_json, "artifact_name"),
        artifact_sha256,
        artifact_json: artifact_json.clone(),
        basis,
        evidence_stage,
        deployment_id: string_field(&artifact_json, "deployment_id"),
        strategy_profile: string_field(&artifact_json, "strategy_profile")
            .unwrap_or_else(|| "unknown".to_string()),
        runtime_score: string_field(&artifact_json, "runtime_score").unwrap_or_default(),
        data_snapshot_id: dataset.data_snapshot_id.clone(),
        dsl_hash,
        factor_name,
        target,
        horizon,
        recording_path: string_field(&artifact_json, "recording_path"),
        recording_sha256: string_field(&artifact_json, "recording_sha256"),
        config_path: string_field(&artifact_json, "config_path"),
        config_sha256: string_field(&artifact_json, "config_sha256"),
        runner_source: string_field(&artifact_json, "runner_source"),
        runner_git_sha: string_field(&artifact_json, "runner_git_sha"),
        decision_contract_json: artifact_json
            .get("decision_contract")
            .cloned()
            .unwrap_or_else(|| json!({})),
        acceptance_criteria_json: artifact_json
            .get("acceptance_criteria")
            .cloned()
            .unwrap_or_else(|| json!({})),
        metrics_json: artifact_json
            .get("metrics")
            .cloned()
            .unwrap_or_else(|| json!({})),
        blocking_risk_flags_json: artifact_json
            .get("blocking_risk_flags")
            .cloned()
            .unwrap_or_else(|| json!([])),
        promotion_ready,
        promotion_decision,
        artifact_path: path.to_path_buf(),
    })
}

fn factor_name_from_runtime_score(value: &Value) -> Option<String> {
    string_field(value, "runtime_score")
        .and_then(|score| {
            score
                .strip_prefix("autofactor_formula:")
                .map(ToOwned::to_owned)
        })
        .filter(|name| !name.is_empty())
}

fn factor_preview_row(
    factor: &Value,
    default_target: &str,
    default_horizon: &str,
    path: &Path,
) -> Result<FactorPreviewRow> {
    let factor_name = string_field(factor, "factor_name")
        .or_else(|| string_field(factor, "name"))
        .with_context(|| format!("{} factor missing factor_name", path.display()))?;
    let target = string_field(factor, "target").unwrap_or_else(|| default_target.to_string());
    let dsl_hash = string_field(factor, "dsl_hash")
        .with_context(|| format!("{} factor {factor_name} missing dsl_hash", path.display()))?;
    let runtime_contract = factor
        .get("runtime_contract")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let ast_json = factor
        .get("ast_json")
        .cloned()
        .or_else(|| runtime_contract.get("ast_json").cloned())
        .unwrap_or_else(|| json!({}));
    let metrics = factor.get("metrics").cloned().unwrap_or_else(|| json!({}));
    let blockers = merged_blockers(factor, &runtime_contract);
    let raw_status = string_field(factor, "status").unwrap_or_else(|| "evaluated".to_string());
    let decision = factor_decision(&raw_status, &blockers);
    let factor_family = runtime_contract
        .get("strategy_family")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("autofactor")
        .to_string();
    let horizon = string_field(factor, "horizon")
        .or_else(|| string_field(&runtime_contract, "horizon"))
        .unwrap_or_else(|| default_horizon.to_string());

    Ok(FactorPreviewRow {
        factor_name,
        target,
        dsl_hash,
        ast_json,
        runtime_contract,
        factor_family,
        horizon,
        registry_status: decision.registry_status,
        promotion_decision: decision.promotion_decision,
        promotion_status: decision.promotion_status,
        passed_gate: decision.passed_gate,
        blockers,
        metrics,
        raw: factor.clone(),
    })
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|raw| !raw.is_empty())
        .map(ToOwned::to_owned)
}

fn merged_blockers(factor: &Value, runtime_contract: &Value) -> Value {
    let mut blockers = Vec::new();
    extend_blockers(&mut blockers, factor.get("blockers"));
    extend_blockers(&mut blockers, runtime_contract.get("blockers"));
    blockers.sort();
    blockers.dedup();
    Value::Array(blockers.into_iter().map(Value::String).collect())
}

fn extend_blockers(out: &mut Vec<String>, maybe_blockers: Option<&Value>) {
    if let Some(items) = maybe_blockers.and_then(Value::as_array) {
        out.extend(
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|item| !item.is_empty())
                .map(ToOwned::to_owned),
        );
    }
}

fn factor_decision(raw_status: &str, blockers: &Value) -> FactorDecision {
    let has_blockers = blockers
        .as_array()
        .map(|items| !items.is_empty())
        .unwrap_or(true);
    match raw_status {
        "candidate" | "passed" if !has_blockers => FactorDecision {
            registry_status: "candidate".to_string(),
            promotion_decision: "continue".to_string(),
            promotion_status: "candidate".to_string(),
            passed_gate: true,
        },
        "rejected" => FactorDecision {
            registry_status: "deprecated".to_string(),
            promotion_decision: "reject".to_string(),
            promotion_status: "rejected".to_string(),
            passed_gate: false,
        },
        "watchlist" if !has_blockers => FactorDecision {
            registry_status: "evaluated".to_string(),
            promotion_decision: "continue".to_string(),
            promotion_status: "watchlist".to_string(),
            passed_gate: false,
        },
        _ if has_blockers => FactorDecision {
            registry_status: "evaluated".to_string(),
            promotion_decision: "blocked".to_string(),
            promotion_status: "blocked".to_string(),
            passed_gate: false,
        },
        _ => FactorDecision {
            registry_status: "evaluated".to_string(),
            promotion_decision: "not_evaluated".to_string(),
            promotion_status: "blocked".to_string(),
            passed_gate: false,
        },
    }
}

fn find_named_files(root: &Path, name: &str) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_named_files(root, name, &mut out)?;
    out.sort();
    Ok(out)
}

fn collect_named_files(path: &Path, name: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    if path.is_file() {
        if path.file_name().and_then(|item| item.to_str()) == Some(name) {
            out.push(path.to_path_buf());
        }
        return Ok(());
    }
    for entry in fs::read_dir(path).with_context(|| format!("read dir {}", path.display()))? {
        let entry = entry?;
        let entry_path = entry.path();
        if entry_path.is_dir() {
            collect_named_files(&entry_path, name, out)?;
        } else if entry_path.file_name().and_then(|item| item.to_str()) == Some(name) {
            out.push(entry_path);
        }
    }
    Ok(())
}

async fn persist_dataset_snapshot(pool: &PgPool, row: &DatasetSnapshotRow) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO research_dataset_snapshots (
            data_snapshot_id,
            snapshot_hash,
            schema_version,
            source_kind,
            dataset_start_ts,
            dataset_end_ts,
            symbols,
            row_counts_json,
            source_surfaces_json,
            input_artifacts_json,
            sampling_json,
            manifest_json
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9::jsonb, $10::jsonb, $11::jsonb, $12::jsonb)
        ON CONFLICT (data_snapshot_id) DO UPDATE SET
            snapshot_hash = EXCLUDED.snapshot_hash,
            schema_version = EXCLUDED.schema_version,
            source_kind = EXCLUDED.source_kind,
            dataset_start_ts = EXCLUDED.dataset_start_ts,
            dataset_end_ts = EXCLUDED.dataset_end_ts,
            symbols = EXCLUDED.symbols,
            row_counts_json = EXCLUDED.row_counts_json,
            source_surfaces_json = EXCLUDED.source_surfaces_json,
            input_artifacts_json = EXCLUDED.input_artifacts_json,
            sampling_json = EXCLUDED.sampling_json,
            manifest_json = EXCLUDED.manifest_json
        "#,
    )
    .bind(&row.data_snapshot_id)
    .bind(&row.snapshot_hash)
    .bind(&row.schema_version)
    .bind(&row.source_kind)
    .bind(row.dataset_start_ts)
    .bind(row.dataset_end_ts)
    .bind(&row.symbols)
    .bind(row.row_counts_json.to_string())
    .bind(row.source_surfaces_json.to_string())
    .bind(row.input_artifacts_json.to_string())
    .bind(row.sampling_json.to_string())
    .bind(row.manifest_json.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_factor_registry(pool: &PgPool, row: &FactorPreviewRow) -> Result<String> {
    let factor_id = Uuid::new_v4().to_string();
    let metadata = json!({
        "source": "alpha_search_factor_registry_preview",
        "metrics": row.metrics,
        "blockers": row.blockers,
        "raw": row.raw,
    });
    let stored_id: String = sqlx::query_scalar(
        r#"
        INSERT INTO factor_registry (
            factor_id,
            factor_name,
            factor_family,
            status,
            hypothesis,
            economic_logic,
            dsl_source,
            dsl_hash,
            ast_json,
            runtime_contract,
            target,
            horizon,
            created_by_agent,
            metadata
        )
        VALUES ($1::uuid, $2, $3, $4, $5, $6, $7, $8, $9::jsonb, $10::jsonb, $11, $12, $13, $14::jsonb)
        ON CONFLICT (dsl_hash, target, horizon) DO UPDATE SET
            factor_name = EXCLUDED.factor_name,
            factor_family = EXCLUDED.factor_family,
            status = CASE
                WHEN factor_registry.status IN ('dry_run', 'approved', 'production')
                    THEN factor_registry.status
                ELSE EXCLUDED.status
            END,
            hypothesis = EXCLUDED.hypothesis,
            dsl_source = EXCLUDED.dsl_source,
            ast_json = EXCLUDED.ast_json,
            runtime_contract = EXCLUDED.runtime_contract,
            target = EXCLUDED.target,
            horizon = EXCLUDED.horizon,
            metadata = EXCLUDED.metadata
        RETURNING factor_id::text
        "#,
    )
    .bind(factor_id)
    .bind(&row.factor_name)
    .bind(&row.factor_family)
    .bind(&row.registry_status)
    .bind(format!(
        "AutoFactor candidate {} for target {}",
        row.factor_name, row.target
    ))
    .bind("")
    .bind(row.dsl_source())
    .bind(&row.dsl_hash)
    .bind(row.ast_json.to_string())
    .bind(row.runtime_contract.to_string())
    .bind(&row.target)
    .bind(&row.horizon)
    .bind(WRITER_AGENT)
    .bind(metadata.to_string())
    .fetch_one(pool)
    .await?;
    Ok(stored_id)
}

async fn upsert_factor_evaluation(
    pool: &PgPool,
    factor_id: &str,
    run_id: &str,
    dataset: &DatasetSnapshotRow,
    row: &FactorPreviewRow,
) -> Result<()> {
    let rejection_reason = row
        .blockers
        .as_array()
        .filter(|items| !items.is_empty())
        .map(|_| "blocked by runtime/data contract".to_string());
    let metrics_json = json!({
        "source": "alpha_search_factor_registry_preview",
        "metrics": row.metrics,
        "raw_status": row.raw.get("status").and_then(Value::as_str),
    });
    let existing_eval_id: Option<String> = sqlx::query_scalar(
        r#"
        SELECT eval_id::text
        FROM factor_evaluations
        WHERE factor_id = $1::uuid
          AND run_id = $2
          AND data_snapshot_id = $3
          AND evidence_stage = $4
          AND evaluation_kind = $5
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(factor_id)
    .bind(run_id)
    .bind(&dataset.data_snapshot_id)
    .bind(EVIDENCE_STAGE)
    .bind(EVALUATION_KIND)
    .fetch_optional(pool)
    .await?;

    if let Some(eval_id) = existing_eval_id {
        sqlx::query(
            r#"
            UPDATE factor_evaluations SET
                dataset_start_ts = $2,
                dataset_end_ts = $3,
                evidence_stage = $4,
                evaluation_kind = $5,
                candidate_replay_id = $6,
                evaluator_version = $7,
                runtime_contract = $8::jsonb,
                passed_gate = $9,
                promotion_decision = $10,
                promotion_status = $11,
                blockers_json = $12::jsonb,
                rejection_reason = $13,
                metrics_json = $14::jsonb
            WHERE eval_id = $1::uuid
            "#,
        )
        .bind(eval_id)
        .bind(dataset.dataset_start_ts)
        .bind(dataset.dataset_end_ts)
        .bind(EVIDENCE_STAGE)
        .bind(EVALUATION_KIND)
        .bind(None::<String>)
        .bind(EVALUATOR_VERSION)
        .bind(row.runtime_contract.to_string())
        .bind(row.passed_gate)
        .bind(&row.promotion_decision)
        .bind(&row.promotion_status)
        .bind(row.blockers.to_string())
        .bind(rejection_reason)
        .bind(metrics_json.to_string())
        .execute(pool)
        .await?;
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO factor_evaluations (
            eval_id,
            factor_id,
            run_id,
            data_snapshot_id,
            dataset_start_ts,
            dataset_end_ts,
            evidence_stage,
            evaluation_kind,
            candidate_replay_id,
            evaluator_version,
            runtime_contract,
            passed_gate,
            promotion_decision,
            promotion_status,
            blockers_json,
            rejection_reason,
            metrics_json
        )
        VALUES ($1::uuid, $2::uuid, $3, $4, $5, $6, $7, $8, $9, $10, $11::jsonb, $12, $13, $14, $15::jsonb, $16, $17::jsonb)
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(factor_id)
    .bind(run_id)
    .bind(&dataset.data_snapshot_id)
    .bind(dataset.dataset_start_ts)
    .bind(dataset.dataset_end_ts)
    .bind(EVIDENCE_STAGE)
    .bind(EVALUATION_KIND)
    .bind(None::<String>)
    .bind(EVALUATOR_VERSION)
    .bind(row.runtime_contract.to_string())
    .bind(row.passed_gate)
    .bind(&row.promotion_decision)
    .bind(&row.promotion_status)
    .bind(row.blockers.to_string())
    .bind(rejection_reason)
    .bind(metrics_json.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_candidate_replay_tape(pool: &PgPool, row: &CandidateReplayTapeRow) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO candidate_replay_tapes (
            candidate_replay_id,
            run_id,
            source_workflow,
            workflow_run_id,
            workflow_run_url,
            artifact_name,
            artifact_sha256,
            artifact_json,
            basis,
            evidence_stage,
            deployment_id,
            strategy_profile,
            runtime_score,
            data_snapshot_id,
            dsl_hash,
            target,
            horizon,
            recording_path,
            recording_sha256,
            config_path,
            config_sha256,
            runner_source,
            runner_git_sha,
            decision_contract_json,
            acceptance_criteria_json,
            metrics_json,
            blocking_risk_flags_json,
            promotion_ready,
            promotion_decision
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, $10, $11, $12, $13, $14,
            $15, $16, $17, $18, $19, $20, $21, $22, $23, $24::jsonb, $25::jsonb,
            $26::jsonb, $27::jsonb, $28, $29
        )
        ON CONFLICT (candidate_replay_id) DO UPDATE SET
            run_id = EXCLUDED.run_id,
            source_workflow = EXCLUDED.source_workflow,
            workflow_run_id = EXCLUDED.workflow_run_id,
            workflow_run_url = EXCLUDED.workflow_run_url,
            artifact_name = EXCLUDED.artifact_name,
            artifact_sha256 = EXCLUDED.artifact_sha256,
            artifact_json = EXCLUDED.artifact_json,
            basis = EXCLUDED.basis,
            evidence_stage = EXCLUDED.evidence_stage,
            deployment_id = EXCLUDED.deployment_id,
            strategy_profile = EXCLUDED.strategy_profile,
            runtime_score = EXCLUDED.runtime_score,
            data_snapshot_id = EXCLUDED.data_snapshot_id,
            dsl_hash = EXCLUDED.dsl_hash,
            target = EXCLUDED.target,
            horizon = EXCLUDED.horizon,
            recording_path = EXCLUDED.recording_path,
            recording_sha256 = EXCLUDED.recording_sha256,
            config_path = EXCLUDED.config_path,
            config_sha256 = EXCLUDED.config_sha256,
            runner_source = EXCLUDED.runner_source,
            runner_git_sha = EXCLUDED.runner_git_sha,
            decision_contract_json = EXCLUDED.decision_contract_json,
            acceptance_criteria_json = EXCLUDED.acceptance_criteria_json,
            metrics_json = EXCLUDED.metrics_json,
            blocking_risk_flags_json = EXCLUDED.blocking_risk_flags_json,
            promotion_ready = EXCLUDED.promotion_ready,
            promotion_decision = EXCLUDED.promotion_decision
        "#,
    )
    .bind(&row.candidate_replay_id)
    .bind(&row.run_id)
    .bind(&row.source_workflow)
    .bind(&row.workflow_run_id)
    .bind(&row.workflow_run_url)
    .bind(&row.artifact_name)
    .bind(&row.artifact_sha256)
    .bind(row.artifact_json.to_string())
    .bind(&row.basis)
    .bind(&row.evidence_stage)
    .bind(&row.deployment_id)
    .bind(&row.strategy_profile)
    .bind(&row.runtime_score)
    .bind(&row.data_snapshot_id)
    .bind(&row.dsl_hash)
    .bind(&row.target)
    .bind(&row.horizon)
    .bind(&row.recording_path)
    .bind(&row.recording_sha256)
    .bind(&row.config_path)
    .bind(&row.config_sha256)
    .bind(&row.runner_source)
    .bind(&row.runner_git_sha)
    .bind(row.decision_contract_json.to_string())
    .bind(row.acceptance_criteria_json.to_string())
    .bind(row.metrics_json.to_string())
    .bind(row.blocking_risk_flags_json.to_string())
    .bind(row.promotion_ready)
    .bind(&row.promotion_decision)
    .execute(pool)
    .await?;
    Ok(())
}

async fn upsert_candidate_replay_evaluation(
    pool: &PgPool,
    row: &CandidateReplayTapeRow,
) -> Result<()> {
    let factor_id = find_candidate_replay_factor_id(pool, row).await?;
    let Some(factor_id) = factor_id else {
        return Ok(());
    };
    let rejection_reason = row
        .blocking_risk_flags_json
        .as_array()
        .filter(|items| !items.is_empty())
        .map(|_| "blocked by candidate replay contract".to_string());
    let metrics_json = json!({
        "source": "candidate_replay_tape",
        "basis": row.basis,
        "metrics": row.metrics_json,
        "acceptance_criteria": row.acceptance_criteria_json,
    });
    let existing_eval_id: Option<String> = sqlx::query_scalar(
        r#"
        SELECT eval_id::text
        FROM factor_evaluations
        WHERE factor_id = $1::uuid
          AND run_id = $2
          AND data_snapshot_id = $3
          AND evidence_stage = $4
          AND evaluation_kind = 'candidate_replay'
          AND candidate_replay_id = $5
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(&factor_id)
    .bind(&row.run_id)
    .bind(&row.data_snapshot_id)
    .bind(&row.evidence_stage)
    .bind(&row.candidate_replay_id)
    .fetch_optional(pool)
    .await?;

    if let Some(eval_id) = existing_eval_id {
        sqlx::query(
            r#"
            UPDATE factor_evaluations SET
                evidence_stage = $2,
                evaluation_kind = 'candidate_replay',
                candidate_replay_id = $3,
                evaluator_version = $4,
                runtime_contract = $5::jsonb,
                passed_gate = $6,
                promotion_decision = $7,
                promotion_status = $8,
                blockers_json = $9::jsonb,
                rejection_reason = $10,
                metrics_json = $11::jsonb
            WHERE eval_id = $1::uuid
            "#,
        )
        .bind(eval_id)
        .bind(&row.evidence_stage)
        .bind(&row.candidate_replay_id)
        .bind(EVALUATOR_VERSION)
        .bind(row.artifact_json.to_string())
        .bind(row.promotion_ready)
        .bind(&row.promotion_decision)
        .bind(candidate_replay_promotion_status(row))
        .bind(row.blocking_risk_flags_json.to_string())
        .bind(rejection_reason)
        .bind(metrics_json.to_string())
        .execute(pool)
        .await?;
        return Ok(());
    }

    sqlx::query(
        r#"
        INSERT INTO factor_evaluations (
            eval_id,
            factor_id,
            run_id,
            data_snapshot_id,
            dataset_start_ts,
            dataset_end_ts,
            evidence_stage,
            evaluation_kind,
            candidate_replay_id,
            evaluator_version,
            runtime_contract,
            passed_gate,
            promotion_decision,
            promotion_status,
            blockers_json,
            rejection_reason,
            metrics_json
        )
        SELECT
            $1::uuid,
            $2::uuid,
            $3,
            $4,
            s.dataset_start_ts,
            s.dataset_end_ts,
            $5,
            'candidate_replay',
            $6,
            $7,
            $8::jsonb,
            $9,
            $10,
            $11,
            $12::jsonb,
            $13,
            $14::jsonb
        FROM research_dataset_snapshots s
        WHERE s.data_snapshot_id = $4
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&factor_id)
    .bind(&row.run_id)
    .bind(&row.data_snapshot_id)
    .bind(&row.evidence_stage)
    .bind(&row.candidate_replay_id)
    .bind(EVALUATOR_VERSION)
    .bind(row.artifact_json.to_string())
    .bind(row.promotion_ready)
    .bind(&row.promotion_decision)
    .bind(candidate_replay_promotion_status(row))
    .bind(row.blocking_risk_flags_json.to_string())
    .bind(rejection_reason)
    .bind(metrics_json.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

async fn find_candidate_replay_factor_id(
    pool: &PgPool,
    row: &CandidateReplayTapeRow,
) -> Result<Option<String>> {
    if let Some(dsl_hash) = &row.dsl_hash {
        let factor_id = sqlx::query_scalar(
            r#"
            SELECT factor_id::text
            FROM factor_registry
            WHERE dsl_hash = $1
              AND ($2::text IS NULL OR target = $2)
              AND ($3::text IS NULL OR horizon = $3)
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(dsl_hash)
        .bind(&row.target)
        .bind(&row.horizon)
        .fetch_optional(pool)
        .await?;
        if factor_id.is_some() {
            return Ok(factor_id);
        }
    }
    if let Some(factor_name) = &row.factor_name {
        return Ok(sqlx::query_scalar(
            r#"
            SELECT factor_id::text
            FROM factor_registry
            WHERE factor_name = $1
              AND ($2::text IS NULL OR target = $2)
              AND ($3::text IS NULL OR horizon = $3)
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(factor_name)
        .bind(&row.target)
        .bind(&row.horizon)
        .fetch_optional(pool)
        .await?);
    }
    Ok(None)
}

fn candidate_replay_promotion_status(row: &CandidateReplayTapeRow) -> &'static str {
    if row.promotion_ready {
        "ready"
    } else {
        "blocked"
    }
}

async fn append_experiment_trace(
    pool: &PgPool,
    run_id: &str,
    data_snapshot_id: Option<&str>,
    dsl_hash: Option<&str>,
    candidate_replay_id: Option<&str>,
    event_type: &str,
    artifact_kind: &str,
    evidence_stage: &str,
    promotion_decision: Option<&str>,
    artifact_path: &Path,
    output_json: &Value,
) -> Result<()> {
    let latest: Option<(String, String)> = sqlx::query_as(
        r#"
        SELECT trace_id::text, hash_current
        FROM experiment_trace
        WHERE run_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(run_id)
    .fetch_optional(pool)
    .await?;
    let (parent_trace_id, hash_prev) = latest
        .map(|(trace_id, hash)| (Some(trace_id), Some(hash)))
        .unwrap_or((None, None));
    let input_json = json!({
        "artifact_path": artifact_path.display().to_string(),
        "artifact_sha256": file_sha256(artifact_path).ok(),
    });
    let hash_current = trace_hash(
        hash_prev.as_deref(),
        run_id,
        event_type,
        WRITER_AGENT,
        &input_json,
        output_json,
    );
    sqlx::query(
        r#"
        INSERT INTO experiment_trace (
            trace_id,
            run_id,
            parent_trace_id,
            event_type,
            data_snapshot_id,
            dsl_hash,
            candidate_replay_id,
            artifact_kind,
            evidence_stage,
            promotion_decision,
            agent_name,
            input_json,
            output_json,
            hash_prev,
            hash_current
        )
        VALUES ($1::uuid, $2, $3::uuid, $4, $5, $6, $7, $8, $9, $10, $11, $12::jsonb, $13::jsonb, $14, $15)
        "#,
    )
    .bind(Uuid::new_v4().to_string())
    .bind(run_id)
    .bind(parent_trace_id)
    .bind(event_type)
    .bind(data_snapshot_id)
    .bind(dsl_hash)
    .bind(candidate_replay_id)
    .bind(artifact_kind)
    .bind(evidence_stage)
    .bind(promotion_decision)
    .bind(WRITER_AGENT)
    .bind(input_json.to_string())
    .bind(output_json.to_string())
    .bind(hash_prev)
    .bind(hash_current)
    .execute(pool)
    .await?;
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_without_blockers_stays_candidate_not_promoted() {
        let decision = factor_decision("candidate", &json!([]));
        assert_eq!(
            decision,
            FactorDecision {
                registry_status: "candidate".to_string(),
                promotion_decision: "continue".to_string(),
                promotion_status: "candidate".to_string(),
                passed_gate: true,
            }
        );
    }

    #[test]
    fn runtime_blockers_fail_closed() {
        let decision = factor_decision("candidate", &json!(["missing_runtime_input"]));
        assert_eq!(decision.registry_status, "evaluated");
        assert_eq!(decision.promotion_decision, "blocked");
        assert_eq!(decision.promotion_status, "blocked");
        assert!(!decision.passed_gate);
    }

    #[test]
    fn rejected_preview_maps_to_deprecated_registry_row() {
        let decision = factor_decision("rejected", &json!([]));
        assert_eq!(decision.registry_status, "deprecated");
        assert_eq!(decision.promotion_decision, "reject");
        assert_eq!(decision.promotion_status, "rejected");
        assert!(!decision.passed_gate);
    }

    #[test]
    fn merges_factor_and_runtime_blockers() {
        let blockers = merged_blockers(
            &json!({"blockers": ["b", "a"]}),
            &json!({"blockers": ["a", "c"]}),
        );
        assert_eq!(blockers, json!(["a", "b", "c"]));
    }

    fn preview_plan(alpha_search_dir: PathBuf) -> TracePlan {
        TracePlan {
            run_id: "test-run".to_string(),
            snapshot_dir: PathBuf::from("/tmp/unused-snapshot"),
            alpha_search_dir: Some(alpha_search_dir),
            registry_json: None,
            promotion_json: None,
            handoff_json: None,
            candidate_replay_jsons: Vec::new(),
            dry_run: true,
        }
    }

    fn write_preview(path: &Path, value: &Value) {
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(
            path,
            serde_json::to_string_pretty(value).expect("serialize preview"),
        )
        .expect("write preview");
    }

    #[test]
    fn reads_versioned_factor_registry_preview_envelope() {
        let root = std::env::temp_dir().join(format!(
            "ploy-trace-preview-envelope-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let preview_path = root
            .join("full_depth_settlement_executable_pnl")
            .join("factor-registry-preview.json");
        write_preview(
            &preview_path,
            &json!({
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [{
                    "factor_name": "auto_settlement_conservative_settlement_edge",
                    "dsl_hash": "abc123",
                    "ast_json": {"Input": "conservative_settlement_edge"},
                    "horizon": "5m",
                    "status": "candidate",
                    "runtime_contract": {
                        "strategy_family": "settlement_probability",
                        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                        "horizon": "5m",
                        "blockers": []
                    },
                    "blockers": [],
                    "metrics": {"reward": 1.0}
                }]
            }),
        );

        let rows = collect_factor_preview_rows(&preview_plan(root.clone())).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, "full_depth_settlement_executable_pnl");
        assert_eq!(rows[0].horizon, "5m");
        assert_eq!(rows[0].factor_family, "settlement_probability");
        assert_eq!(rows[0].promotion_decision, "continue");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_legacy_top_level_array_preview_and_uses_parent_target() {
        let root = std::env::temp_dir().join(format!(
            "ploy-trace-preview-legacy-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let preview_path = root
            .join("full_depth_reprice_pnl_10s")
            .join("factor-registry-preview.json");
        write_preview(
            &preview_path,
            &json!([{
                "factor_name": "legacy_factor",
                "dsl_hash": "legacy123",
                "ast_json": {"Input": "external_move_since_poly_update"},
                "status": "watchlist",
                "blockers": [],
                "metrics": {"reward": 0.2}
            }]),
        );

        let rows = collect_factor_preview_rows(&preview_plan(root.clone())).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].target, "full_depth_reprice_pnl_10s");
        assert_eq!(rows[0].horizon, "10s");
        assert_eq!(rows[0].promotion_status, "watchlist");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_preview_without_factors_still_fails_closed() {
        let root = std::env::temp_dir().join(format!(
            "ploy-trace-preview-bad-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let preview_path = root
            .join("full_depth_settlement_executable_pnl")
            .join("factor-registry-preview.json");
        write_preview(
            &preview_path,
            &json!({"target": "full_depth_settlement_executable_pnl"}),
        );

        let error = collect_factor_preview_rows(&preview_plan(root.clone()))
            .expect_err("bad preview should fail");
        assert!(error.to_string().contains("missing factors array"));
        let _ = fs::remove_dir_all(root);
    }
}
