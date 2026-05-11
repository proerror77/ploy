//! factor_review_v2 — execution-aware PM5D factor review
//!
//! Loads official-settlement historical data, builds the legacy
//! `FactorObservation` rows, derives side-aware `FactorObservationV2` rows, and
//! prints data-health plus single-factor execution metrics.
//!
//! Usage:
//!   cargo run -p ploy-research --features db --example factor_review_v2 -- \
//!     --db-url postgres://... \
//!     --symbols BTCUSDT,ETHUSDT,SOLUSDT \
//!     --start-date 2026-04-24 \
//!     --end-date 2026-04-24 \
//!     --stake-usd 15 \
//!     [--lob-sample-secs 5] \
//!     [--observation-sample-secs 30] \
//!     [--max-quote-age-secs 30] \
//!     [--top-n 20] \
//!     [--git-ref main] \
//!     [--output-json artifacts/factor-review-v2/evaluation.json]

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_market_contracts::MarketUpdate;
use ploy_research::{
    FactorObservation, FactorReviewOptions, FactorReviewV2Report, ResearchSnapshotManifest,
    ResearchSnapshotRequest, build_factor_observations_with_lob_sampled,
    format_factor_review_v2_report, load_deribit_feature_snapshots,
    load_research_lob_snapshots_sampled, load_research_pm_book_snapshots_sampled,
    load_research_snapshot, review_factors_v2_with_deribit_and_pm_books_filtered,
    validate_snapshot_request_coverage,
};
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;
use std::{fs::File, path::Path, path::PathBuf, time::Duration};

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn flag_present(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    let next_day = date
        .succ_opt()
        .unwrap_or_else(|| panic!("invalid end date: {raw}"));
    Utc.from_utc_datetime(&next_day.and_hms_opt(0, 0, 0).unwrap())
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    raw.parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
}

#[derive(Debug, Serialize)]
struct FactorReviewWindow {
    start: DateTime<Utc>,
    end_exclusive: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct FactorReviewAccountingContract {
    stake_usd: f64,
    settlement_basis: &'static str,
    fillability_basis: &'static str,
    pnl_basis: &'static str,
    cost_basis: &'static str,
}

#[derive(Debug, Serialize)]
struct FactorReviewV2Artifact<'a> {
    schema_version: u32,
    artifact_type: &'static str,
    producer: &'static str,
    generated_at: DateTime<Utc>,
    git_ref: Option<&'a str>,
    window: FactorReviewWindow,
    symbols: &'a [String],
    accounting_contract: FactorReviewAccountingContract,
    canonical_result: &'a str,
    factor_name_filter: Option<&'a str>,
    risk_flags: Vec<String>,
    snapshot_manifest: Option<&'a ResearchSnapshotManifest>,
    report: &'a FactorReviewV2Report,
}

fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) <= end);
    &items[lo..hi]
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url");
    let start = flag_value(&args, "--start-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_start(&flag_value(&args, "--start-date").expect("--start-date required"))
        });
    let end = flag_value(&args, "--end-ts")
        .map(|raw| parse_timestamp(&raw))
        .unwrap_or_else(|| {
            parse_date_end(&flag_value(&args, "--end-date").expect("--end-date required"))
        });
    let symbols: Vec<String> = flag_value(&args, "--symbols")
        .unwrap_or_else(|| "BTCUSDT".to_string())
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let top_n: usize = flag_value(&args, "--top-n")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(20);
    let git_ref = flag_value(&args, "--git-ref");
    let output_json = flag_value(&args, "--output-json").map(PathBuf::from);
    let factor_name_filter = flag_value(&args, "--factor-name-filter")
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty());
    let options = FactorReviewOptions {
        stake_usd: flag_value(&args, "--stake-usd")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(15.0),
        min_observations: flag_value(&args, "--min-observations")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        top_quantile: flag_value(&args, "--top-quantile")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(0.2),
    };

    eprintln!(
        "factor_review_v2: {} -> {} for {:?}, stake_usd={:.2}, observation_sample_secs={}, factor_name_filter={}",
        start,
        end,
        symbols,
        options.stake_usd,
        observation_sample_secs,
        factor_name_filter.as_deref().unwrap_or("<none>")
    );

    let snapshot_dir = flag_value(&args, "--snapshot-dir");
    let allow_direct_db_debug = flag_present(&args, "--allow-direct-db-debug");
    if snapshot_dir.is_some() && db_url.is_some() {
        eprintln!("ERROR: --snapshot-dir cannot be combined with --db-url");
        std::process::exit(2);
    }
    let mut snapshot_provenance: Option<String> = None;
    let mut snapshot_manifest: Option<ResearchSnapshotManifest> = None;
    let (observations, deribit_snapshots, all_pm_book_snapshots): (
        Vec<FactorObservation>,
        Vec<_>,
        Vec<_>,
    ) = if let Some(snapshot_dir) = snapshot_dir {
        let started = std::time::Instant::now();
        let snapshot =
            load_research_snapshot(&snapshot_dir).expect("load research snapshot failed");
        validate_snapshot_request_coverage(
            &snapshot.manifest,
            ResearchSnapshotRequest {
                symbols: &symbols,
                start,
                end,
                lob_sample_secs,
                observation_sample_secs,
                max_quote_age_secs,
                stake_usd: options.stake_usd,
                require_official_settlement: true,
            },
        )
        .expect("snapshot does not cover requested review inputs");
        let snapshot_hash = snapshot
            .manifest
            .snapshot_hash
            .as_deref()
            .unwrap_or("<missing>");
        eprintln!(
            "snapshot: schema={} hash={} generated_at={} observations={} deribit={} pm_books={} load_ms={}",
            snapshot.manifest.schema_version,
            snapshot_hash,
            snapshot.manifest.generated_at,
            snapshot.observations.len(),
            snapshot.deribit_snapshots.len(),
            snapshot.pm_book_snapshots.len(),
            started.elapsed().as_millis()
        );
        snapshot_provenance = Some(format!(
            "# Snapshot\nsnapshot_schema={}\nsnapshot_hash={}\nsnapshot_generated_at={}\nsnapshot_optimizer_data_dir={}\nsnapshot_data_requirements={}\nsnapshot_data_audit_status={}\nsnapshot_data_audit_report={}\nsnapshot_include_deribit={}\n",
            snapshot.manifest.schema_version,
            snapshot_hash,
            snapshot.manifest.generated_at,
            snapshot
                .manifest
                .optimizer_data_dir
                .as_deref()
                .unwrap_or("<missing>"),
            if snapshot.manifest.data_requirements.is_empty() {
                "<unspecified>".to_string()
            } else {
                snapshot.manifest.data_requirements.join(",")
            },
            snapshot
                .manifest
                .data_audit_status
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot
                .manifest
                .data_audit_report
                .as_deref()
                .unwrap_or("<not-recorded>"),
            snapshot.manifest.include_deribit
        ));
        snapshot_manifest = Some(snapshot.manifest.clone());
        (
            snapshot.observations,
            snapshot.deribit_snapshots,
            snapshot.pm_book_snapshots,
        )
    } else {
        if !allow_direct_db_debug {
            eprintln!(
                "ERROR: direct DB factor review is non-canonical; pass --snapshot-dir or explicit --allow-direct-db-debug"
            );
            std::process::exit(2);
        }
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(120))
            .connect(
                db_url
                    .as_deref()
                    .expect("--db-url required without --snapshot-dir"),
            )
            .await
            .expect("database connection failed");

        let mut phase_timings = Vec::new();
        let historical_sample_secs = u32::try_from(lob_sample_secs.max(1)).unwrap_or(1);
        let started = std::time::Instant::now();
        let all_updates = load_from_database_with_options(
            &pool,
            &symbols,
            start,
            end,
            &HistoricalLoadOptions {
                require_official_settlement: true,
                include_l2: false,
                spot_sample_secs: historical_sample_secs,
                lob_sample_secs: historical_sample_secs,
                ..Default::default()
            },
        )
        .await
        .expect("bulk historical load failed");
        phase_timings.push((
            "historical_updates",
            started.elapsed().as_millis(),
            all_updates.len(),
        ));
        eprintln!("updates: {}", all_updates.len());

        let started = std::time::Instant::now();
        let all_lob_snapshots =
            load_research_lob_snapshots_sampled(&pool, &symbols, start, end, lob_sample_secs)
                .await
                .expect("bulk lob snapshot load failed");
        phase_timings.push((
            "cex_lob_snapshots",
            started.elapsed().as_millis(),
            all_lob_snapshots.len(),
        ));
        eprintln!("lob_snapshots: {}", all_lob_snapshots.len());

        let started = std::time::Instant::now();
        let all_pm_book_snapshots =
            load_research_pm_book_snapshots_sampled(&pool, &symbols, start, end, lob_sample_secs)
                .await
                .expect("bulk PM book snapshot load failed");
        phase_timings.push((
            "pm_book_snapshots",
            started.elapsed().as_millis(),
            all_pm_book_snapshots.len(),
        ));
        eprintln!("pm_book_snapshots: {}", all_pm_book_snapshots.len());

        let started = std::time::Instant::now();
        let deribit_snapshots =
            load_deribit_feature_snapshots(&pool, &symbols, start, end, observation_sample_secs)
                .await;
        phase_timings.push((
            "deribit_snapshots",
            started.elapsed().as_millis(),
            deribit_snapshots.len(),
        ));
        eprintln!("deribit_snapshots: {}", deribit_snapshots.len());

        let updates_slice_start =
            start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
        let updates_slice = slice_by_time(
            &all_updates,
            updates_slice_start,
            end,
            MarketUpdate::sort_ts,
        );
        let lob_slice = slice_by_time(&all_lob_snapshots, start, end, |snapshot| snapshot.ts);

        let started = std::time::Instant::now();
        let observations: Vec<FactorObservation> = build_factor_observations_with_lob_sampled(
            updates_slice,
            lob_slice,
            max_quote_age_secs,
            observation_sample_secs,
        );
        phase_timings.push((
            "factor_observations",
            started.elapsed().as_millis(),
            observations.len(),
        ));
        for (phase, elapsed_ms, rows) in phase_timings {
            eprintln!("phase {phase:<24} {elapsed_ms:>8} ms rows={rows}");
        }
        eprintln!("factor_observations: {}", observations.len());
        (observations, deribit_snapshots, all_pm_book_snapshots)
    };
    let canonical_result = if snapshot_manifest.is_some() {
        "snapshot"
    } else {
        "direct_db_debug"
    };

    if observations.is_empty() {
        eprintln!("no observations — check date range, symbols, quote coverage, and settlements");
        return;
    }

    let report = review_factors_v2_with_deribit_and_pm_books_filtered(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        options,
        factor_name_filter.as_deref(),
    );
    if let Some(output_json) = output_json.as_deref() {
        write_factor_review_artifact(
            output_json,
            &report,
            start,
            end,
            &symbols,
            git_ref.as_deref(),
            canonical_result,
            factor_name_filter.as_deref(),
            snapshot_manifest.as_ref(),
        );
    }
    if let Some(snapshot_provenance) = snapshot_provenance {
        println!("{snapshot_provenance}");
    }
    println!("{}", format_factor_review_v2_report(&report, top_n));
}

fn write_factor_review_artifact(
    path: &Path,
    report: &FactorReviewV2Report,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    symbols: &[String],
    git_ref: Option<&str>,
    canonical_result: &str,
    factor_name_filter: Option<&str>,
    snapshot_manifest: Option<&ResearchSnapshotManifest>,
) {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|err| panic!("create output dir {} failed: {err}", parent.display()));
    }
    let artifact = FactorReviewV2Artifact {
        schema_version: 1,
        artifact_type: "factor_review_v2_evaluation",
        producer: "factor_review_v2",
        generated_at: Utc::now(),
        git_ref,
        window: FactorReviewWindow {
            start,
            end_exclusive: end,
        },
        symbols,
        accounting_contract: FactorReviewAccountingContract {
            stake_usd: report.options.stake_usd,
            settlement_basis: "official_polymarket_settlement_required",
            fillability_basis: "prefer_full_depth_entry_fillable_else_top_book",
            pnl_basis: "prefer_full_depth_executable_pnl_15u_else_top_book_executable_pnl_15u",
            cost_basis: "entry_crypto_fee_plus_full_depth_sweep_slippage_when_available",
        },
        canonical_result,
        factor_name_filter,
        risk_flags: factor_review_risk_flags(report, canonical_result, snapshot_manifest),
        snapshot_manifest,
        report,
    };
    let file =
        File::create(path).unwrap_or_else(|err| panic!("create {} failed: {err}", path.display()));
    serde_json::to_writer_pretty(file, &artifact)
        .unwrap_or_else(|err| panic!("write {} failed: {err}", path.display()));
    eprintln!("factor_review_v2_artifact={}", path.display());
}

fn factor_review_risk_flags(
    report: &FactorReviewV2Report,
    canonical_result: &str,
    snapshot_manifest: Option<&ResearchSnapshotManifest>,
) -> Vec<String> {
    let mut flags = Vec::new();
    if canonical_result != "snapshot" {
        flags.push("non_canonical_direct_db_debug".to_string());
    }
    if snapshot_manifest.is_some_and(|manifest| !manifest.require_official_settlement) {
        flags.push("snapshot_without_official_settlement".to_string());
    }
    if report.health.v2_rows == 0 {
        flags.push("no_v2_rows".to_string());
    }
    if report.health.executable_pnl_rows == 0 && report.health.full_depth_executable_pnl_rows == 0 {
        flags.push("no_executable_pnl_rows".to_string());
    }
    if report.health.full_depth_entry_fill_rate() < 0.05 {
        flags.push("low_full_depth_entry_fill_rate".to_string());
    }
    if !report
        .executable_ev_buckets
        .iter()
        .any(|bucket| bucket.statistically_supported)
    {
        flags.push("no_statistically_supported_positive_ev_bucket".to_string());
    }
    if report
        .executable_ev_buckets
        .iter()
        .any(|bucket| bucket.positive_ev)
        && !report
            .executable_ev_buckets
            .iter()
            .any(|bucket| bucket.positive_ev && !bucket.underpowered)
    {
        flags.push("positive_ev_buckets_underpowered_only".to_string());
    }
    flags
}
