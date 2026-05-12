//! factor_walk_forward_v2 — executable PM5D factor walk-forward review
//!
//! The training window fits each factor's direction and selected-quantile
//! threshold. The following test window only applies that trained threshold and
//! scores executable PnL after PM CLOB fillability.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{load_from_database_with_options, HistoricalLoadOptions};
use ploy_market_contracts::MarketUpdate;
use ploy_research::{
    autofactor_matrix_from_v2, build_factor_observations_v2_with_deribit_and_pm_books,
    build_factor_observations_with_lob_sampled, build_factor_stability_report,
    build_full_depth_execution_matrix, build_settlement_probability_promotion_gate_report,
    format_autofactor_reports, format_factor_combo_v1_report, format_factor_stability_report,
    format_factor_walk_forward_v2_report, format_fillability_review_v1_report,
    format_full_depth_execution_matrix_report, format_liquidity_gate_v1_report,
    format_liquidity_gated_alpha_v1_report, format_meta_label_walk_forward_v1_report,
    format_repricing_ic_report, format_settlement_probability_promotion_gate_report,
    format_settlement_probability_report, format_settlement_probability_walk_forward_report,
    format_trade_formation_v1_report, liquidity_gate_v1_with_deribit_and_pm_books,
    liquidity_gated_alpha_v1_with_deribit_and_pm_books, load_deribit_feature_snapshots,
    load_research_lob_snapshots_sampled, load_research_pm_book_snapshots_sampled,
    load_research_snapshot, mine_domain_autofactors_from_v2_with_guidance, read_mcts_search_state,
    review_fillability_v1_with_deribit_and_pm_books, review_repricing_ic_with_deribit_and_pm_books,
    review_trade_formation_v1_with_deribit_and_pm_books, validate_snapshot_request_coverage,
    walk_forward_factor_combo_v1_with_deribit_and_pm_books,
    walk_forward_factors_v2_with_deribit_and_pm_books,
    walk_forward_meta_label_v1_with_deribit_and_pm_books, write_alpha_search_artifacts_with_state,
    AutoFactorOptions, AutoFactorV2Target, FactorComboV1Options, FactorObservation,
    FactorReviewOptions, FactorStabilityOptions, FactorWalkForwardOptions,
    FillabilityReviewOptions, FullDepthExecutionMatrixOptions, LiquidityGateV1Options,
    LiquidityGatedAlphaV1Options, LlmPriorSpec, MetaLabelWalkForwardOptions, RepricingIcOptions,
    ResearchSnapshotRequest, SettlementProbabilityDataQualityMode,
    SettlementProbabilityPromotionGateOptions, SettlementProbabilityReportOptions,
    SettlementProbabilityWalkForwardOptions, TradeFormationReviewOptions,
};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashSet;
use std::time::Duration;

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

fn parse_data_quality_mode(raw: Option<String>) -> SettlementProbabilityDataQualityMode {
    match raw.as_deref().unwrap_or("strict_continuous") {
        "strict_continuous" | "strict-continuous" => {
            SettlementProbabilityDataQualityMode::StrictContinuous
        }
        "event_complete" | "event-complete" => SettlementProbabilityDataQualityMode::EventComplete,
        value => panic!("invalid --data-quality-mode: {value}"),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReportSuite {
    Core,
    Full,
}

impl ReportSuite {
    fn parse(raw: Option<String>) -> Self {
        match raw.as_deref().unwrap_or("full") {
            "core" => Self::Core,
            "full" => Self::Full,
            value => panic!("invalid --report-suite: {value}"),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Full => "full",
        }
    }
}

fn replay_parity_evidence(path: &str) -> (bool, String) {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read replay parity JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse replay parity JSON {path} failed: {err}"));
    let runtime_ready = json
        .pointer("/runtime_evidence_comparison/strict_parity_ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let event_ready = json
        .pointer("/event_comparison/strict_parity_ready")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let risk_flags = json
        .get("blocking_risk_flags")
        .or_else(|| json.get("risk_flags"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let advisory_flags = json
        .get("advisory_flags")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let decision = json
        .get("decision")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>");
    let ready = runtime_ready && event_ready && risk_flags.is_empty() && decision == "continue";
    let evidence = format!(
        "replay_parity_json={} runtime_ready={} event_ready={} blocking_flags={} advisory_flags={} decision={}",
        path,
        runtime_ready,
        event_ready,
        if risk_flags.is_empty() {
            "<none>".to_string()
        } else {
            risk_flags.join("|")
        },
        if advisory_flags.is_empty() {
            "<none>".to_string()
        } else {
            advisory_flags.join("|")
        },
        decision,
    );
    (ready, evidence)
}

#[cfg(test)]
mod tests {
    use super::replay_parity_evidence;
    use std::fs;

    fn write_parity_fixture(name: &str, payload: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "ploy-factor-walk-forward-{name}-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(&path, payload).expect("write parity fixture");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn replay_parity_requires_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "event-not-ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": false}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(!ready);
        assert!(evidence.contains("runtime_ready=true"));
        assert!(evidence.contains("event_ready=false"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn replay_parity_accepts_runtime_and_event_strict_readiness() {
        let path = write_parity_fixture(
            "ready",
            r#"{
              "decision": "continue",
              "blocking_risk_flags": [],
              "runtime_evidence_comparison": {"strict_parity_ready": true},
              "event_comparison": {"strict_parity_ready": true}
            }"#,
        );

        let (ready, evidence) = replay_parity_evidence(&path);

        assert!(ready, "{evidence}");
        let _ = fs::remove_file(path);
    }
}

fn alpha_search_plan_factor_names(path: &str) -> Vec<String> {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search plan JSON {path} failed: {err}"));
    let json: serde_json::Value = serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search plan JSON {path} failed: {err}"));
    json.get("selected_nodes")
        .and_then(serde_json::Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get("factor_name"))
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn read_llm_prior(path: &str) -> LlmPriorSpec {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("read alpha search LLM prior JSON {path} failed: {err}"));
    serde_json::from_str(&raw)
        .unwrap_or_else(|err| panic!("parse alpha search LLM prior JSON {path} failed: {err}"))
}

fn slice_by_time<T, F>(items: &[T], start: DateTime<Utc>, end: DateTime<Utc>, ts_fn: F) -> &[T]
where
    F: Fn(&T) -> DateTime<Utc>,
{
    let lo = items.partition_point(|item| ts_fn(item) < start);
    let hi = items.partition_point(|item| ts_fn(item) < end);
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
        .unwrap_or(30);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let observation_sample_secs: i64 = flag_value(&args, "--observation-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    let review = FactorReviewOptions {
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
    let options = FactorWalkForwardOptions {
        review,
        train_window_days: flag_value(&args, "--train-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(2),
        test_window_days: flag_value(&args, "--test-window-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        step_days: flag_value(&args, "--step-days")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(1),
        top_n: flag_value(&args, "--top-n")
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(20),
        factor_name_filter: flag_value(&args, "--factor-name-filter"),
    };
    let report_suite = ReportSuite::parse(flag_value(&args, "--report-suite"));

    eprintln!(
        "factor_walk_forward_v2: {} -> {} for {:?}, stake_usd={:.2}, train_days={}, test_days={}, observation_sample_secs={}, factor_name_filter={}, report_suite={}",
        start,
        end,
        symbols,
        options.review.stake_usd,
        options.train_window_days,
        options.test_window_days,
        observation_sample_secs,
        options.factor_name_filter.as_deref().unwrap_or("<none>"),
        report_suite.as_str()
    );

    let snapshot_dir = flag_value(&args, "--snapshot-dir");
    let replay_parity_json = flag_value(&args, "--replay-parity-json");
    let alpha_search_output_dir = flag_value(&args, "--alpha-search-output-dir");
    let alpha_search_plan_json = flag_value(&args, "--alpha-search-plan-json");
    let alpha_search_llm_prior_json = flag_value(&args, "--alpha-search-llm-prior-json");
    let alpha_search_state_json = flag_value(&args, "--alpha-search-state-json");
    let allow_direct_db_debug = flag_present(&args, "--allow-direct-db-debug");
    let data_quality_mode = parse_data_quality_mode(flag_value(&args, "--data-quality-mode"));
    let min_event_complete_events = flag_value(&args, "--min-event-complete-events")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(20);
    let min_event_complete_rows = flag_value(&args, "--min-event-complete-rows")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(40);
    if snapshot_dir.is_some() && db_url.is_some() {
        eprintln!("ERROR: --snapshot-dir cannot be combined with --db-url");
        std::process::exit(2);
    }
    let mut snapshot_provenance: Option<String> = None;
    let snapshot_data_audit_status: Option<String>;
    let include_deribit: bool;
    let (observations, deribit_snapshots, all_pm_book_snapshots): (
        Vec<FactorObservation>,
        Vec<_>,
        Vec<_>,
    ) = if let Some(snapshot_dir) = snapshot_dir {
        let started = std::time::Instant::now();
        let snapshot =
            load_research_snapshot(&snapshot_dir).expect("load research snapshot failed");
        let snapshot_symbol_set: HashSet<&str> = snapshot
            .manifest
            .symbols
            .iter()
            .map(String::as_str)
            .collect();
        let missing_symbols: Vec<&str> = symbols
            .iter()
            .map(String::as_str)
            .filter(|symbol| !snapshot_symbol_set.contains(symbol))
            .collect();
        if !missing_symbols.is_empty() {
            eprintln!(
                "ERROR: requested symbols {:?} are not present in snapshot symbols {:?}",
                missing_symbols, snapshot.manifest.symbols
            );
            std::process::exit(2);
        }
        let validation_symbols = snapshot.manifest.symbols.clone();
        validate_snapshot_request_coverage(
            &snapshot.manifest,
            ResearchSnapshotRequest {
                symbols: &validation_symbols,
                start,
                end,
                lob_sample_secs,
                observation_sample_secs,
                max_quote_age_secs,
                stake_usd: options.review.stake_usd,
                require_official_settlement: true,
            },
        )
        .expect("snapshot does not cover requested walk-forward inputs");
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
        snapshot_data_audit_status = snapshot.manifest.data_audit_status.clone();
        include_deribit = snapshot.manifest.include_deribit;
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
        let requested_symbol_set: HashSet<&str> = symbols.iter().map(String::as_str).collect();
        let mut observations: Vec<FactorObservation> = snapshot
            .observations
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .collect();
        let event_ids: HashSet<String> = observations
            .iter()
            .map(|row| row.event_id.clone())
            .collect();
        let deribit_snapshots: Vec<_> = snapshot
            .deribit_snapshots
            .into_iter()
            .filter(|row| requested_symbol_set.contains(row.symbol.as_str()))
            .collect();
        let pm_book_snapshots: Vec<_> = snapshot
            .pm_book_snapshots
            .into_iter()
            .filter(|row| event_ids.contains(&row.event_id))
            .collect();
        observations.shrink_to_fit();
        eprintln!(
            "snapshot filtered: requested_symbols={:?} observations={} deribit={} pm_books={}",
            symbols,
            observations.len(),
            deribit_snapshots.len(),
            pm_book_snapshots.len()
        );
        (observations, deribit_snapshots, pm_book_snapshots)
    } else {
        if !allow_direct_db_debug {
            eprintln!(
                "ERROR: direct DB factor walk-forward is non-canonical; pass --snapshot-dir or explicit --allow-direct-db-debug"
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
        let history_start = start - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
        let historical_sample_secs = u32::try_from(lob_sample_secs.max(1)).unwrap_or(1);
        let started = std::time::Instant::now();
        let all_updates = load_from_database_with_options(
            &pool,
            &symbols,
            history_start,
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
        let all_lob_snapshots = load_research_lob_snapshots_sampled(
            &pool,
            &symbols,
            history_start,
            end,
            lob_sample_secs,
        )
        .await
        .expect("bulk lob snapshot load failed");
        phase_timings.push((
            "cex_lob_snapshots",
            started.elapsed().as_millis(),
            all_lob_snapshots.len(),
        ));
        eprintln!("lob_snapshots: {}", all_lob_snapshots.len());

        let started = std::time::Instant::now();
        let all_pm_book_snapshots = load_research_pm_book_snapshots_sampled(
            &pool,
            &symbols,
            history_start,
            end,
            lob_sample_secs,
        )
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

        let updates_slice = slice_by_time(&all_updates, history_start, end, MarketUpdate::sort_ts);
        let lob_slice = slice_by_time(&all_lob_snapshots, history_start, end, |snapshot| {
            snapshot.ts
        });

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
        snapshot_data_audit_status = None;
        include_deribit = !deribit_snapshots.is_empty();
        (observations, deribit_snapshots, all_pm_book_snapshots)
    };

    if observations.is_empty() {
        eprintln!("no observations — check date range, symbols, quote coverage, and settlements");
        return;
    }

    let report = walk_forward_factors_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        options.clone(),
    );
    if let Some(snapshot_provenance) = snapshot_provenance {
        println!("{snapshot_provenance}");
    }
    println!("{}", format_factor_walk_forward_v2_report(&report));
    let execution_matrix = build_full_depth_execution_matrix(
        &observations,
        &all_pm_book_snapshots,
        FullDepthExecutionMatrixOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&execution_matrix, options.top_n)
    );
    let conservative_execution_matrix = build_full_depth_execution_matrix(
        &observations,
        &all_pm_book_snapshots,
        FullDepthExecutionMatrixOptions {
            visible_depth_haircut: 0.5,
            max_levels: Some(3),
            min_bucket_observations: options.review.min_observations.max(20),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_full_depth_execution_matrix_report(&conservative_execution_matrix, options.top_n)
    );
    if report_suite == ReportSuite::Full {
        let repricing_ic_report = review_repricing_ic_with_deribit_and_pm_books(
            &observations,
            &deribit_snapshots,
            &all_pm_book_snapshots,
            RepricingIcOptions {
                review: options.review.clone(),
                ..Default::default()
            },
        );
        println!(
            "{}",
            format_repricing_ic_report(&repricing_ic_report, options.top_n)
        );
    }
    let autofactor_rows = build_factor_observations_v2_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        &options.review,
    );
    let settlement_probability_report = ploy_research::build_settlement_probability_report(
        &autofactor_rows,
        SettlementProbabilityReportOptions {
            min_bucket_observations: options.review.min_observations.max(20),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_report(&settlement_probability_report)
    );
    let settlement_probability_walk_forward_report =
        ploy_research::walk_forward_settlement_probability_report(
            &autofactor_rows,
            start,
            end,
            SettlementProbabilityWalkForwardOptions {
                walk_forward: options.clone(),
                probability: SettlementProbabilityReportOptions {
                    min_bucket_observations: options.review.min_observations.max(20),
                    ..Default::default()
                },
            },
        );
    println!(
        "{}",
        format_settlement_probability_walk_forward_report(
            &settlement_probability_walk_forward_report
        )
    );
    let event_complete_rows = autofactor_rows
        .iter()
        .filter(|row| {
            row.label_full_depth_entry_fillable
                && row.label_conservative_entry_fillable
                && row.label_settlement_win.is_some()
                && row.label_full_depth_executable_pnl_15u.is_some()
                && row.label_conservative_executable_pnl_15u.is_some()
                && (!include_deribit || row.deribit_mark_iv.is_finite())
        })
        .collect::<Vec<_>>();
    let event_complete_events = event_complete_rows
        .iter()
        .map(|row| row.event_id.as_str())
        .collect::<HashSet<_>>()
        .len();
    let replay_parity_status = replay_parity_json.as_deref().map(replay_parity_evidence);
    let promotion_gate_report = build_settlement_probability_promotion_gate_report(
        &settlement_probability_report,
        &settlement_probability_walk_forward_report,
        &execution_matrix,
        &conservative_execution_matrix,
        SettlementProbabilityPromotionGateOptions {
            stake_usd: options.review.stake_usd,
            include_deribit,
            data_audit_status: snapshot_data_audit_status,
            data_quality_mode,
            event_complete_events,
            event_complete_rows: event_complete_rows.len(),
            min_event_complete_events,
            min_event_complete_rows,
            replay_parity_ready: replay_parity_status
                .as_ref()
                .map(|(ready, _)| *ready)
                .unwrap_or(false),
            replay_parity_evidence: replay_parity_status.map(|(_, evidence)| evidence),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_settlement_probability_promotion_gate_report(&promotion_gate_report)
    );
    let autofactor_options = AutoFactorOptions {
        min_observations: options.review.min_observations.max(50),
        min_window_observations: options.review.min_observations.max(20),
        ..Default::default()
    };
    let alpha_search_input_names = alpha_search_output_dir.as_ref().and_then(|_| {
        autofactor_matrix_from_v2(&autofactor_rows)
            .map(|matrix| matrix.input_names().into_iter().collect::<Vec<_>>())
            .map_err(|err| {
                eprintln!("alpha search matrix build failed: {err}");
                err
            })
            .ok()
    });
    let alpha_search_plan_names = alpha_search_plan_json
        .as_deref()
        .map(alpha_search_plan_factor_names)
        .unwrap_or_default();
    let llm_prior = alpha_search_llm_prior_json.as_deref().map(read_llm_prior);
    if let Some(path) = alpha_search_llm_prior_json.as_deref() {
        eprintln!("alpha search typed LLM prior loaded from {path}");
    }
    let mcts_state = alpha_search_state_json.as_deref().map(|path| {
        read_mcts_search_state(path)
            .unwrap_or_else(|err| panic!("read alpha search MCTS state JSON {path} failed: {err}"))
    });
    if let Some(path) = alpha_search_state_json.as_deref() {
        eprintln!("alpha search cumulative MCTS state loaded from {path}");
    }
    if let Some(path) = alpha_search_plan_json.as_deref() {
        eprintln!(
            "alpha search MCTS plan loaded: {} selected nodes from {}",
            alpha_search_plan_names.len(),
            path
        );
    }
    for target in [
        AutoFactorV2Target::FullDepthRepricePnl10s,
        AutoFactorV2Target::FullDepthRepricePnl30s,
        AutoFactorV2Target::FullDepthSettlementExecutablePnl,
    ] {
        match mine_domain_autofactors_from_v2_with_guidance(
            &autofactor_rows,
            target,
            &autofactor_options,
            &alpha_search_plan_names,
            llm_prior.as_ref(),
        ) {
            Ok(reports) => {
                println!("# AutoFactor target={}", target.as_str());
                println!("{}", format_autofactor_reports(&reports, options.top_n));
                if let (Some(output_dir), Some(input_names)) = (
                    alpha_search_output_dir.as_deref(),
                    alpha_search_input_names.as_ref(),
                ) {
                    match write_alpha_search_artifacts_with_state(
                        output_dir,
                        target.as_str(),
                        input_names,
                        &reports,
                        &autofactor_options,
                        mcts_state.as_ref(),
                    ) {
                        Ok(summary) => eprintln!(
                            "alpha search artifacts written target={} candidates={} rejected={} best={} dir={}",
                            summary.target,
                            summary.candidate_count,
                            summary.rejected_count,
                            summary.best_candidate.as_deref().unwrap_or("<none>"),
                            summary.output_dir
                        ),
                        Err(err) => {
                            eprintln!(
                                "alpha search artifact write failed for {}: {err}",
                                target.as_str()
                            );
                        }
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "autofactor seed report failed for {}: {err}",
                    target.as_str()
                );
            }
        }
    }
    if report_suite == ReportSuite::Core {
        return;
    }
    let fillability_report = review_fillability_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        FillabilityReviewOptions {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_fillability_review_v1_report(&fillability_report, options.top_n)
    );
    let liquidity_gate_report = liquidity_gate_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        LiquidityGateV1Options {
            review: options.review.clone(),
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_liquidity_gate_v1_report(&liquidity_gate_report)
    );
    let gated_alpha_report = liquidity_gated_alpha_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        LiquidityGatedAlphaV1Options {
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            walk_forward: options.clone(),
        },
    );
    println!(
        "{}",
        format_liquidity_gated_alpha_v1_report(&gated_alpha_report, options.top_n)
    );
    let formation_report = review_trade_formation_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        TradeFormationReviewOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!("{}", format_trade_formation_v1_report(&formation_report));
    let meta_label_report = walk_forward_meta_label_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        MetaLabelWalkForwardOptions {
            review: options.review.clone(),
            gate: LiquidityGateV1Options {
                review: options.review.clone(),
                ..Default::default()
            },
            train_window_days: options.train_window_days,
            test_window_days: options.test_window_days,
            step_days: options.step_days,
            top_n: options.top_n,
            ..Default::default()
        },
    );
    println!(
        "{}",
        format_meta_label_walk_forward_v1_report(&meta_label_report)
    );
    let stability_report =
        build_factor_stability_report(&report, FactorStabilityOptions::default());
    println!(
        "{}",
        format_factor_stability_report(&stability_report, options.top_n)
    );
    let combo_report = walk_forward_factor_combo_v1_with_deribit_and_pm_books(
        &observations,
        &deribit_snapshots,
        &all_pm_book_snapshots,
        start,
        end,
        FactorComboV1Options {
            walk_forward: options,
            ..Default::default()
        },
    );
    println!("{}", format_factor_combo_v1_report(&combo_report));
}
