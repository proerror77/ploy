use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
#[cfg(feature = "db")]
use std::time::Instant;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::research_os::feature_store::{
    feature_snapshot_manifest_from_research_snapshot, FEATURE_SNAPSHOT_MANIFEST_ARTIFACT,
};
use crate::{DeribitFeatureSnapshot, FactorObservation, ResearchPmBookSnapshot};

pub const RESEARCH_SNAPSHOT_SCHEMA_VERSION: &str = "research_snapshot_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotArtifacts {
    pub observations_json: String,
    pub deribit_snapshots_json: String,
    pub pm_book_snapshots_json: String,
    pub quality_markdown: String,
    pub query_timings_json: String,
    #[serde(default = "default_feature_snapshot_manifest_json")]
    pub feature_snapshot_manifest_json: String,
    pub observations_parquet: Option<String>,
}

impl Default for ResearchSnapshotArtifacts {
    fn default() -> Self {
        Self {
            observations_json: "observations.json".to_string(),
            deribit_snapshots_json: "deribit_snapshots.json".to_string(),
            pm_book_snapshots_json: "pm_book_snapshots.json".to_string(),
            quality_markdown: "quality.md".to_string(),
            query_timings_json: "query_timings.json".to_string(),
            feature_snapshot_manifest_json: default_feature_snapshot_manifest_json(),
            observations_parquet: None,
        }
    }
}

fn default_feature_snapshot_manifest_json() -> String {
    FEATURE_SNAPSHOT_MANIFEST_ARTIFACT.to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResearchSnapshotRowCounts {
    pub observations: usize,
    #[serde(default)]
    pub lob_snapshots: usize,
    pub deribit_snapshots: usize,
    pub pm_book_snapshots: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotPhaseTiming {
    pub phase: String,
    pub elapsed_ms: u128,
    pub rows: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResearchSnapshotManifest {
    pub schema_version: String,
    pub snapshot_hash: Option<String>,
    pub generated_at: DateTime<Utc>,
    pub git_sha: Option<String>,
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub history_start: DateTime<Utc>,
    pub lob_sample_secs: i32,
    #[serde(default)]
    pub pm_book_sample_secs: Option<i32>,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub immutable_input: bool,
    pub source_kind: String,
    pub optimizer_data_dir: Option<String>,
    #[serde(default)]
    pub data_requirements: Vec<String>,
    #[serde(default)]
    pub data_audit_status: Option<String>,
    #[serde(default)]
    pub data_audit_report: Option<String>,
    #[serde(default = "default_include_deribit")]
    pub include_deribit: bool,
    pub artifacts: ResearchSnapshotArtifacts,
    pub row_counts: ResearchSnapshotRowCounts,
    pub phase_timings: Vec<ResearchSnapshotPhaseTiming>,
    pub quality_flags: Vec<String>,
}

fn default_include_deribit() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct ResearchSnapshot {
    pub manifest: ResearchSnapshotManifest,
    pub observations: Vec<FactorObservation>,
    pub deribit_snapshots: Vec<DeribitFeatureSnapshot>,
    pub pm_book_snapshots: Vec<ResearchPmBookSnapshot>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResearchSnapshotRequest<'a> {
    pub symbols: &'a [String],
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
}

pub fn load_research_snapshot(snapshot_dir: impl AsRef<Path>) -> Result<ResearchSnapshot> {
    let snapshot_dir = snapshot_dir.as_ref();
    let manifest: ResearchSnapshotManifest =
        read_json(snapshot_dir.join("manifest.json")).context("read research snapshot manifest")?;

    if manifest.schema_version != RESEARCH_SNAPSHOT_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported research snapshot schema {}; expected {}",
            manifest.schema_version,
            RESEARCH_SNAPSHOT_SCHEMA_VERSION
        );
    }

    let observations = read_json(snapshot_dir.join(&manifest.artifacts.observations_json))
        .context("read snapshot observations")?;
    let deribit_snapshots =
        read_json(snapshot_dir.join(&manifest.artifacts.deribit_snapshots_json))
            .context("read snapshot Deribit rows")?;
    let pm_book_snapshots =
        read_json(snapshot_dir.join(&manifest.artifacts.pm_book_snapshots_json))
            .context("read snapshot PM book rows")?;

    Ok(ResearchSnapshot {
        manifest,
        observations,
        deribit_snapshots,
        pm_book_snapshots,
    })
}

pub fn write_research_snapshot(
    snapshot_dir: impl AsRef<Path>,
    mut snapshot: ResearchSnapshot,
) -> Result<ResearchSnapshotManifest> {
    if snapshot.manifest.optimizer_data_dir.is_none() {
        anyhow::bail!("research snapshot manifest requires optimizer_data_dir");
    }

    let snapshot_dir = snapshot_dir.as_ref();
    fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot dir {}", snapshot_dir.display()))?;

    snapshot.manifest.row_counts.observations = snapshot.observations.len();
    snapshot.manifest.row_counts.deribit_snapshots = snapshot.deribit_snapshots.len();
    snapshot.manifest.row_counts.pm_book_snapshots = snapshot.pm_book_snapshots.len();

    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.observations_json),
        &snapshot.observations,
    )?;
    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.deribit_snapshots_json),
        &snapshot.deribit_snapshots,
    )?;
    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.pm_book_snapshots_json),
        &snapshot.pm_book_snapshots,
    )?;

    #[cfg(feature = "polars-export")]
    {
        let parquet_name = "observations.parquet";
        crate::export_observations_parquet(
            &snapshot.observations,
            &snapshot_dir.join(parquet_name),
        )
        .context("write snapshot observations parquet")?;
        snapshot.manifest.artifacts.observations_parquet = Some(parquet_name.to_string());
    }

    snapshot.manifest.snapshot_hash =
        Some(compute_snapshot_hash(snapshot_dir, &snapshot.manifest)?);

    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.query_timings_json),
        &snapshot.manifest.phase_timings,
    )?;
    write_quality_markdown(
        &snapshot_dir.join(&snapshot.manifest.artifacts.quality_markdown),
        &snapshot.manifest,
    )?;
    let feature_manifest = feature_snapshot_manifest_from_research_snapshot(&snapshot.manifest);
    write_json(
        snapshot_dir.join(&snapshot.manifest.artifacts.feature_snapshot_manifest_json),
        &feature_manifest,
    )?;
    write_json(snapshot_dir.join("manifest.json"), &snapshot.manifest)?;

    Ok(snapshot.manifest)
}

pub fn validate_snapshot_request(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, true)
}

pub fn validate_snapshot_request_coverage(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
) -> Result<()> {
    validate_snapshot_request_with_window_mode(manifest, request, false)
}

fn validate_snapshot_request_with_window_mode(
    manifest: &ResearchSnapshotManifest,
    request: ResearchSnapshotRequest<'_>,
    require_exact_window: bool,
) -> Result<()> {
    let mut requested_symbols = request.symbols.to_vec();
    requested_symbols.sort();
    let mut snapshot_symbols = manifest.symbols.clone();
    snapshot_symbols.sort();
    if requested_symbols != snapshot_symbols {
        anyhow::bail!(
            "snapshot symbols {:?} do not match requested symbols {:?}",
            snapshot_symbols,
            requested_symbols
        );
    }
    if require_exact_window && (manifest.start != request.start || manifest.end != request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not match requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if !require_exact_window && (manifest.start > request.start || manifest.end < request.end) {
        anyhow::bail!(
            "snapshot window {} -> {} does not cover requested window {} -> {}",
            manifest.start,
            manifest.end,
            request.start,
            request.end
        );
    }
    if manifest.lob_sample_secs != request.lob_sample_secs {
        anyhow::bail!(
            "snapshot lob_sample_secs {} does not match requested {}",
            manifest.lob_sample_secs,
            request.lob_sample_secs
        );
    }
    if manifest.observation_sample_secs != request.observation_sample_secs {
        anyhow::bail!(
            "snapshot observation_sample_secs {} does not match requested {}",
            manifest.observation_sample_secs,
            request.observation_sample_secs
        );
    }
    if manifest.max_quote_age_secs != request.max_quote_age_secs {
        anyhow::bail!(
            "snapshot max_quote_age_secs {} does not match requested {}",
            manifest.max_quote_age_secs,
            request.max_quote_age_secs
        );
    }
    if (manifest.stake_usd - request.stake_usd).abs() > 1e-9 {
        anyhow::bail!(
            "snapshot stake_usd {} does not match requested {}",
            manifest.stake_usd,
            request.stake_usd
        );
    }
    if manifest.require_official_settlement != request.require_official_settlement {
        anyhow::bail!(
            "snapshot require_official_settlement {} does not match requested {}",
            manifest.require_official_settlement,
            request.require_official_settlement
        );
    }
    if !manifest.immutable_input {
        anyhow::bail!("snapshot manifest is not marked immutable_input=true");
    }
    if manifest
        .snapshot_hash
        .as_deref()
        .unwrap_or_default()
        .is_empty()
    {
        anyhow::bail!("snapshot manifest is missing snapshot_hash");
    }
    Ok(())
}

#[cfg(feature = "db")]
pub struct ResearchSnapshotBuildOptions {
    pub symbols: Vec<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub lob_sample_secs: i32,
    pub pm_book_sample_secs: i32,
    pub observation_sample_secs: i64,
    pub max_quote_age_secs: i64,
    pub stake_usd: f64,
    pub require_official_settlement: bool,
    pub optimizer_data_dir: Option<String>,
    pub git_sha: Option<String>,
    pub data_requirements: Vec<String>,
    pub data_audit_status: Option<String>,
    pub data_audit_report: Option<String>,
    pub include_deribit: bool,
}

#[cfg(feature = "db")]
pub async fn build_research_snapshot_from_database(
    pool: &sqlx::PgPool,
    options: ResearchSnapshotBuildOptions,
) -> Result<ResearchSnapshot> {
    use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
    use ploy_market_contracts::MarketUpdate;

    use crate::{
        build_factor_observations_with_lob_sampled, load_deribit_feature_snapshots_with_timings,
        load_research_lob_snapshots_sampled, load_research_pm_book_snapshots_sampled,
    };

    let mut phase_timings = Vec::new();
    let history_start = options.start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
    let historical_sample_secs = u32::try_from(options.lob_sample_secs.max(1)).unwrap_or(1);

    let started = Instant::now();
    let all_updates = load_from_database_with_options(
        pool,
        &options.symbols,
        history_start,
        options.end,
        &HistoricalLoadOptions {
            require_official_settlement: options.require_official_settlement,
            include_l2: false,
            spot_sample_secs: historical_sample_secs,
            lob_sample_secs: historical_sample_secs,
            ..Default::default()
        },
    )
    .await
    .context("load historical market updates")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "historical_updates".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_updates.len()),
    });

    let started = Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        options.lob_sample_secs,
    )
    .await
    .context("load CEX LOB snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "cex_lob_snapshots".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_lob_snapshots.len()),
    });

    let started = Instant::now();
    let pm_book_sample_secs = options.pm_book_sample_secs.max(1);
    let all_pm_book_snapshots = load_research_pm_book_snapshots_sampled(
        pool,
        &options.symbols,
        history_start,
        options.end,
        pm_book_sample_secs,
    )
    .await
    .context("load PM book snapshots")?;
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "pm_book_snapshots".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(all_pm_book_snapshots.len()),
    });

    let deribit_snapshots = if options.include_deribit {
        let started = Instant::now();
        let deribit_result = load_deribit_feature_snapshots_with_timings(
            pool,
            &options.symbols,
            options.start,
            options.end,
            options.observation_sample_secs,
        )
        .await;
        let deribit_snapshots = deribit_result.snapshots;
        phase_timings.extend(deribit_result.phase_timings);
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots".to_string(),
            elapsed_ms: started.elapsed().as_millis(),
            rows: Some(deribit_snapshots.len()),
        });
        deribit_snapshots
    } else {
        phase_timings.push(ResearchSnapshotPhaseTiming {
            phase: "deribit_snapshots_skipped".to_string(),
            elapsed_ms: 0,
            rows: Some(0),
        });
        Vec::new()
    };

    let started = Instant::now();
    let updates_slice = slice_by_time(
        &all_updates,
        history_start,
        options.end,
        MarketUpdate::sort_ts,
    );
    let lob_slice = slice_by_time(&all_lob_snapshots, history_start, options.end, |snapshot| {
        snapshot.ts
    });
    let observations = build_factor_observations_with_lob_sampled(
        updates_slice,
        lob_slice,
        options.max_quote_age_secs,
        options.observation_sample_secs,
    );
    phase_timings.push(ResearchSnapshotPhaseTiming {
        phase: "factor_observations".to_string(),
        elapsed_ms: started.elapsed().as_millis(),
        rows: Some(observations.len()),
    });

    let mut quality_flags = Vec::new();
    if observations.is_empty() {
        quality_flags.push("no_factor_observations".to_string());
    }
    if all_lob_snapshots.is_empty() {
        quality_flags.push("no_lob_snapshots".to_string());
    }
    if options.include_deribit && deribit_snapshots.is_empty() {
        quality_flags.push("no_deribit_snapshots".to_string());
    }
    if all_pm_book_snapshots.is_empty() {
        quality_flags.push("no_pm_book_snapshots".to_string());
    }

    Ok(ResearchSnapshot {
        manifest: ResearchSnapshotManifest {
            schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
            snapshot_hash: None,
            generated_at: Utc::now(),
            git_sha: options.git_sha,
            symbols: options.symbols,
            start: options.start,
            end: options.end,
            history_start,
            lob_sample_secs: options.lob_sample_secs,
            pm_book_sample_secs: Some(pm_book_sample_secs),
            observation_sample_secs: options.observation_sample_secs,
            max_quote_age_secs: options.max_quote_age_secs,
            stake_usd: options.stake_usd,
            require_official_settlement: options.require_official_settlement,
            immutable_input: true,
            source_kind: "tango_postgres_compiled_snapshot".to_string(),
            optimizer_data_dir: options.optimizer_data_dir,
            data_requirements: options.data_requirements,
            data_audit_status: options.data_audit_status,
            data_audit_report: options.data_audit_report,
            include_deribit: options.include_deribit,
            artifacts: ResearchSnapshotArtifacts::default(),
            row_counts: ResearchSnapshotRowCounts {
                observations: observations.len(),
                lob_snapshots: all_lob_snapshots.len(),
                deribit_snapshots: deribit_snapshots.len(),
                pm_book_snapshots: all_pm_book_snapshots.len(),
            },
            phase_timings,
            quality_flags,
        },
        observations,
        deribit_snapshots,
        pm_book_snapshots: all_pm_book_snapshots,
    })
}

#[cfg(feature = "db")]
fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) < end);
    &items[lo..hi]
}

fn read_json<T: for<'de> Deserialize<'de>>(path: PathBuf) -> Result<T> {
    let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    serde_json::from_reader(BufReader::new(file))
        .with_context(|| format!("parse {}", path.display()))
}

fn write_json<T: Serialize>(path: PathBuf, value: &T) -> Result<()> {
    let file = File::create(&path).with_context(|| format!("create {}", path.display()))?;
    serde_json::to_writer_pretty(file, value).with_context(|| format!("write {}", path.display()))
}

fn compute_snapshot_hash(
    snapshot_dir: &Path,
    manifest: &ResearchSnapshotManifest,
) -> Result<String> {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    fn update(hash: &mut u64, bytes: &[u8]) {
        for byte in bytes {
            *hash ^= u64::from(*byte);
            *hash = hash.wrapping_mul(FNV_PRIME);
        }
    }

    let mut hash = FNV_OFFSET;
    update(&mut hash, RESEARCH_SNAPSHOT_SCHEMA_VERSION.as_bytes());
    update(&mut hash, manifest.start.to_rfc3339().as_bytes());
    update(&mut hash, manifest.end.to_rfc3339().as_bytes());
    update(&mut hash, manifest.symbols.join(",").as_bytes());
    update(&mut hash, manifest.stake_usd.to_string().as_bytes());
    update(
        &mut hash,
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_default()
            .as_bytes(),
    );
    update(&mut hash, manifest.data_requirements.join(",").as_bytes());
    update(&mut hash, manifest.include_deribit.to_string().as_bytes());
    update(
        &mut hash,
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    update(
        &mut hash,
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("")
            .as_bytes(),
    );
    for artifact in [
        &manifest.artifacts.observations_json,
        &manifest.artifacts.deribit_snapshots_json,
        &manifest.artifacts.pm_book_snapshots_json,
    ] {
        update(&mut hash, artifact.as_bytes());
        let bytes = fs::read(snapshot_dir.join(artifact))
            .with_context(|| format!("read snapshot artifact {artifact} for hashing"))?;
        update(&mut hash, &bytes);
    }
    Ok(format!("{hash:016x}"))
}

fn write_quality_markdown(path: &Path, manifest: &ResearchSnapshotManifest) -> Result<()> {
    let mut body = String::new();
    body.push_str("# Research Snapshot Quality\n\n");
    body.push_str(&format!("- Schema: `{}`\n", manifest.schema_version));
    body.push_str(&format!(
        "- Snapshot hash: `{}`\n",
        manifest.snapshot_hash.as_deref().unwrap_or("<missing>")
    ));
    body.push_str(&format!("- Generated at: `{}`\n", manifest.generated_at));
    body.push_str(&format!(
        "- Window: `{}` -> `{}`\n",
        manifest.start, manifest.end
    ));
    body.push_str(&format!("- Symbols: `{}`\n", manifest.symbols.join(",")));
    body.push_str(&format!(
        "- LOB sample secs: `{}`\n",
        manifest.lob_sample_secs
    ));
    body.push_str(&format!(
        "- PM book sample secs: `{}`\n",
        manifest
            .pm_book_sample_secs
            .map(|value| value.to_string())
            .unwrap_or_else(|| "<same-as-lob>".to_string())
    ));
    body.push_str(&format!(
        "- Observation sample secs: `{}`\n",
        manifest.observation_sample_secs
    ));
    body.push_str(&format!(
        "- Immutable input: `{}`\n",
        manifest.immutable_input
    ));
    body.push_str(&format!("- Source kind: `{}`\n", manifest.source_kind));
    body.push_str(&format!(
        "- Optimizer data dir: `{}`\n",
        manifest
            .optimizer_data_dir
            .as_deref()
            .unwrap_or("<missing>")
    ));
    body.push_str(&format!(
        "- Data requirements: `{}`\n",
        if manifest.data_requirements.is_empty() {
            "<unspecified>".to_string()
        } else {
            manifest.data_requirements.join(",")
        }
    ));
    body.push_str(&format!(
        "- Data audit status: `{}`\n",
        manifest
            .data_audit_status
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Data audit report: `{}`\n",
        manifest
            .data_audit_report
            .as_deref()
            .unwrap_or("<not-recorded>")
    ));
    body.push_str(&format!(
        "- Deribit included: `{}`\n",
        manifest.include_deribit
    ));
    body.push_str(&format!(
        "- Rows: observations={}, lob={}, deribit={}, pm_books={}\n",
        manifest.row_counts.observations,
        manifest.row_counts.lob_snapshots,
        manifest.row_counts.deribit_snapshots,
        manifest.row_counts.pm_book_snapshots
    ));
    body.push_str(&format!(
        "- Official settlement required: `{}`\n",
        manifest.require_official_settlement
    ));
    body.push_str("\n## Phase Timings\n\n");
    for timing in &manifest.phase_timings {
        body.push_str(&format!(
            "- `{}`: {} ms, rows={}\n",
            timing.phase,
            timing.elapsed_ms,
            timing
                .rows
                .map(|rows| rows.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        ));
    }
    if !manifest.quality_flags.is_empty() {
        body.push_str("\n## Quality Flags\n\n");
        for flag in &manifest.quality_flags {
            body.push_str(&format!("- `{flag}`\n"));
        }
    }
    fs::write(path, body).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_load_empty_snapshot_roundtrips_manifest() {
        let root = std::env::temp_dir().join(format!(
            "ploy-research-snapshot-test-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let start = "2026-04-24T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-05-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_start = "2026-04-25T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let subset_end = "2026-04-27T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let snapshot = ResearchSnapshot {
            manifest: ResearchSnapshotManifest {
                schema_version: RESEARCH_SNAPSHOT_SCHEMA_VERSION.to_string(),
                snapshot_hash: None,
                generated_at: Utc::now(),
                git_sha: Some("test-sha".to_string()),
                symbols: vec!["BTCUSDT".to_string()],
                start,
                end,
                history_start: start,
                lob_sample_secs: 30,
                pm_book_sample_secs: Some(30),
                observation_sample_secs: 30,
                max_quote_age_secs: 30,
                stake_usd: 15.0,
                require_official_settlement: true,
                immutable_input: true,
                source_kind: "unit_test".to_string(),
                optimizer_data_dir: Some("/tmp/immutable-parquet".to_string()),
                data_requirements: vec!["polymarket_quotes".to_string()],
                data_audit_status: Some("ok".to_string()),
                data_audit_report: Some("data-gap-audit.json".to_string()),
                include_deribit: false,
                artifacts: ResearchSnapshotArtifacts::default(),
                row_counts: ResearchSnapshotRowCounts::default(),
                phase_timings: vec![ResearchSnapshotPhaseTiming {
                    phase: "unit".to_string(),
                    elapsed_ms: 1,
                    rows: Some(0),
                }],
                quality_flags: vec![],
            },
            observations: vec![],
            deribit_snapshots: vec![],
            pm_book_snapshots: vec![],
        };

        let written = write_research_snapshot(&root, snapshot).expect("write snapshot");
        let loaded = load_research_snapshot(&root).expect("load snapshot");
        assert_eq!(written.schema_version, RESEARCH_SNAPSHOT_SCHEMA_VERSION);
        assert!(written.snapshot_hash.is_some());
        assert_eq!(loaded.manifest.git_sha.as_deref(), Some("test-sha"));
        validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: loaded.manifest.start,
                end: loaded.manifest.end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot request validation");
        validate_snapshot_request_coverage(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        )
        .expect("snapshot coverage validation");
        let exact_subset_result = validate_snapshot_request(
            &loaded.manifest,
            ResearchSnapshotRequest {
                symbols: &["BTCUSDT".to_string()],
                start: subset_start,
                end: subset_end,
                lob_sample_secs: loaded.manifest.lob_sample_secs,
                observation_sample_secs: loaded.manifest.observation_sample_secs,
                max_quote_age_secs: loaded.manifest.max_quote_age_secs,
                stake_usd: loaded.manifest.stake_usd,
                require_official_settlement: loaded.manifest.require_official_settlement,
            },
        );
        assert!(exact_subset_result.is_err());
        assert_eq!(loaded.manifest.row_counts.observations, 0);
        assert!(root.join("quality.md").exists());
        assert!(root.join(FEATURE_SNAPSHOT_MANIFEST_ARTIFACT).exists());

        let _ = std::fs::remove_dir_all(root);
    }
}
