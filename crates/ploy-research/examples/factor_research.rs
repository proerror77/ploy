use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use ploy_feed_loaders::{HistoricalLoadOptions, load_from_database_with_options};
use ploy_market_contracts::MarketUpdate;
use ploy_research::factors::{pearson_ic, spearman_ic};
use ploy_research::{
    DatasetSourceWindow, EventMetadataChronologyInput, EventRootDatasetBuildRequest,
    FactorObservation, aggregate_factor_metrics, build_event_root_dataset, build_event_summaries,
    build_factor_observations_with_lob, export_event_root_dataset_parquet, factor_metrics,
    load_research_lob_snapshots_sampled, standard_event_root_dataset_artifacts,
};
use sqlx::postgres::PgPoolOptions;
use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

#[derive(Debug, Clone, sqlx::FromRow)]
struct ValidWindowRow {
    symbol: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
    event_count: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct EventMetadataRow {
    market_slug: String,
    symbol: String,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
}

fn csv_values(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EntryTarget {
    label: String,
    seconds: i64,
    tolerance_secs: i64,
}

fn format_entry_label(seconds: i64) -> String {
    if seconds == 0 {
        "@last".to_string()
    } else if seconds % 60 == 0 {
        format!("@{}m", seconds / 60)
    } else {
        format!("@{}s", seconds)
    }
}

fn parse_entry_target_token(token: &str) -> Vec<i64> {
    let value = token.trim();
    if value.is_empty() {
        return Vec::new();
    }

    if let Ok(seconds) = value.parse::<i64>() {
        return vec![seconds];
    }

    let parts: Vec<_> = value.split(':').map(str::trim).collect();
    if parts.len() != 3 {
        panic!("invalid entry target token: {value}");
    }

    let start = parts[0]
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("invalid range start seconds: {}", parts[0]));
    let end = parts[1]
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("invalid range end seconds: {}", parts[1]));
    let step = parts[2]
        .parse::<i64>()
        .unwrap_or_else(|_| panic!("invalid range step seconds: {}", parts[2]));

    if step <= 0 {
        panic!("entry target range step must be positive: {value}");
    }

    let mut out = Vec::new();
    if start >= end {
        let mut current = start;
        while current >= end {
            out.push(current);
            current -= step;
        }
    } else {
        let mut current = start;
        while current <= end {
            out.push(current);
            current += step;
        }
    }
    out
}

fn build_entry_targets(
    raw_targets: Option<&str>,
    shared_tolerance_secs: Option<i64>,
) -> Vec<EntryTarget> {
    let mut seconds: Vec<i64> = raw_targets
        .unwrap_or("290:10:10,10:1:1,0")
        .split(',')
        .flat_map(parse_entry_target_token)
        .collect();

    if seconds.is_empty() {
        panic!("entry targets must not be empty");
    }

    seconds.sort_unstable_by(|lhs, rhs| rhs.cmp(lhs));
    seconds.dedup();

    seconds
        .iter()
        .enumerate()
        .map(|(idx, seconds_value)| {
            let tolerance_secs = if *seconds_value == 0 {
                0
            } else if let Some(shared) = shared_tolerance_secs {
                shared
            } else {
                let mut nearest_gap = i64::MAX;
                if idx > 0 {
                    nearest_gap = nearest_gap.min(seconds[idx - 1] - seconds[idx]);
                }
                if idx + 1 < seconds.len() {
                    nearest_gap = nearest_gap.min(seconds[idx] - seconds[idx + 1]);
                }
                let derived = if nearest_gap == i64::MAX {
                    30
                } else {
                    nearest_gap / 2
                };
                derived.clamp(2, 30)
            };

            EntryTarget {
                label: format_entry_label(*seconds_value),
                seconds: *seconds_value,
                tolerance_secs,
            }
        })
        .collect()
}

fn parse_date_start(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("valid start timestamp"))
}

fn parse_date_end(raw: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .unwrap_or_else(|_| panic!("invalid date: {raw}"));
    Utc.from_utc_datetime(&date.and_hms_opt(23, 59, 59).expect("valid end timestamp"))
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|_| panic!("invalid timestamp: {raw}"))
        .with_timezone(&Utc)
}

async fn discover_valid_windows(
    pool: &sqlx::PgPool,
    symbols: &[String],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    max_windows: i64,
) -> Vec<ValidWindowRow> {
    // Try the pre-computed materialized view first (fast path).
    // Falls back to the raw 3-way join if the matview doesn't exist yet.
    let matview_result: Result<Vec<ValidWindowRow>, _> = sqlx::query_as(
        r#"
        SELECT
            symbol,
            start_time,
            end_time,
            1::bigint AS event_count
        FROM research_valid_windows
        WHERE symbol = ANY($1)
          AND start_time >= $2
          AND end_time <= $3
        ORDER BY start_time
        LIMIT $4
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(max_windows)
    .fetch_all(pool)
    .await;

    if let Ok(rows) = matview_result {
        eprintln!("discover_valid_windows: used matview ({} rows)", rows.len());
        return rows;
    }

    eprintln!("discover_valid_windows: matview not available, using raw query");
    sqlx::query_as(
        r#"
        SELECT
            m.symbol,
            m.start_time,
            m.end_time,
            COUNT(*)::bigint AS event_count
        FROM pm_market_metadata m
        WHERE m.symbol = ANY($1)
          AND m.start_time >= $2
          AND m.end_time <= $3
          AND EXTRACT(EPOCH FROM (m.end_time - m.start_time)) = 300
          AND EXISTS (
              SELECT 1
              FROM pm_token_settlements s
              WHERE s.market_slug = m.market_slug
                AND s.resolved = true
          )
          AND EXISTS (
              SELECT 1
              FROM binance_lob_ticks l
              WHERE l.symbol = m.symbol
                AND l.event_time >= m.start_time
                AND l.event_time <= m.end_time
          )
        GROUP BY m.symbol, m.start_time, m.end_time
        ORDER BY m.start_time
        LIMIT $4
        "#,
    )
    .bind(symbols)
    .bind(start)
    .bind(end)
    .bind(max_windows)
    .fetch_all(pool)
    .await
    .expect("valid window discovery failed")
}

async fn load_event_chronology_inputs(
    pool: &sqlx::PgPool,
    observations: &[FactorObservation],
) -> Vec<EventMetadataChronologyInput> {
    let event_ids = observed_event_ids(observations);

    if event_ids.is_empty() {
        return Vec::new();
    }

    let rows: Vec<EventMetadataRow> = sqlx::query_as(
        r#"
        SELECT
            market_slug,
            symbol,
            start_time,
            end_time
        FROM pm_market_metadata
        WHERE market_slug = ANY($1)
          AND EXTRACT(EPOCH FROM (end_time - start_time)) = 300
        ORDER BY end_time, symbol, market_slug
        "#,
    )
    .bind(&event_ids)
    .fetch_all(pool)
    .await
    .expect("event dataset metadata query failed");

    event_metadata_rows_to_chronology_inputs(&event_ids, rows)
}

fn observed_event_ids(observations: &[FactorObservation]) -> Vec<String> {
    observations
        .iter()
        .map(|row| row.event_id.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn event_metadata_rows_to_chronology_inputs(
    event_ids: &[String],
    rows: Vec<EventMetadataRow>,
) -> Vec<EventMetadataChronologyInput> {
    let mut by_event_id = BTreeMap::new();
    for row in rows {
        if by_event_id.insert(row.market_slug.clone(), row).is_some() {
            panic!("duplicate pm_market_metadata row for observed event id");
        }
    }

    let missing_event_ids: Vec<_> = event_ids
        .iter()
        .filter(|event_id| !by_event_id.contains_key(event_id.as_str()))
        .cloned()
        .collect();
    if !missing_event_ids.is_empty() {
        panic!(
            "missing canonical pm_market_metadata rows for observed event ids: {}",
            missing_event_ids.join(",")
        );
    }

    event_ids
        .iter()
        .map(|event_id| {
            let row = by_event_id
                .remove(event_id.as_str())
                .expect("event metadata presence was checked");
            EventMetadataChronologyInput {
                event_id: event_id.clone(),
                symbol: row.symbol,
                start_time: Some(row.start_time),
                end_time: Some(row.end_time),
            }
        })
        .collect()
}

/// Slices a sorted slice to items whose timestamp falls in `[start, end]`.
/// Precondition: `items` must be sorted by `ts_fn` in ascending order.
fn slice_by_time<'a, T>(
    items: &'a [T],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    ts_fn: impl Fn(&T) -> DateTime<Utc>,
) -> &'a [T] {
    let lo = items.partition_point(|x| ts_fn(x) < start);
    let hi = items.partition_point(|x| ts_fn(x) <= end);
    &items[lo..hi]
}

fn descending_abs_f64_cmp(lhs: f64, rhs: f64) -> Ordering {
    let lhs_key = if lhs.is_finite() {
        lhs.abs()
    } else {
        f64::NEG_INFINITY
    };
    let rhs_key = if rhs.is_finite() {
        rhs.abs()
    } else {
        f64::NEG_INFINITY
    };
    rhs_key.total_cmp(&lhs_key)
}

#[derive(Debug, Clone)]
struct TimeBucketMetric {
    label: String,
    factor: String,
    bucket_start_secs: i64,
    bucket_end_secs: i64,
    n: usize,
    pearson_ic: f64,
    spearman_ic: f64,
}

#[derive(Debug, Clone)]
struct TimeRegime {
    name: &'static str,
    start_secs: i64,
    end_secs: i64,
}

#[derive(Debug, Clone)]
struct RegimeFactorSummary {
    regime_name: &'static str,
    label: String,
    factor: String,
    mean_abs_spearman: f64,
    mean_signed_spearman: f64,
    total_n: usize,
    strongest_bucket_start_secs: i64,
    strongest_bucket_end_secs: i64,
    strongest_bucket_spearman: f64,
}

fn default_time_regimes(max_secs: i64) -> Vec<TimeRegime> {
    let max_secs = max_secs.max(0);
    let mut regimes = Vec::new();

    // early: first 30s of the event (time_remaining 271-300s)
    let early_start = 271.min(max_secs);
    if max_secs >= early_start {
        regimes.push(TimeRegime {
            name: "early",
            start_secs: early_start,
            end_secs: max_secs,
        });
    }

    // middle: 30s after start through last 60s (time_remaining 61-270s)
    let middle_end = max_secs.min(270);
    if middle_end >= 61 {
        regimes.push(TimeRegime {
            name: "middle",
            start_secs: 61,
            end_secs: middle_end,
        });
    }

    // expiry: last 60s — no trading, just for IC analysis
    let expiry_end = max_secs.min(60);
    regimes.push(TimeRegime {
        name: "expiry",
        start_secs: 0,
        end_secs: expiry_end,
    });

    regimes
}

fn summarize_regime_factors(
    metrics: &[TimeBucketMetric],
    regimes: &[TimeRegime],
) -> Vec<RegimeFactorSummary> {
    let mut grouped: BTreeMap<(&'static str, String, String), Vec<&TimeBucketMetric>> =
        BTreeMap::new();

    for metric in metrics
        .iter()
        .filter(|metric| metric.spearman_ic.is_finite())
    {
        for regime in regimes {
            if metric.bucket_start_secs >= regime.start_secs
                && metric.bucket_end_secs <= regime.end_secs
            {
                grouped
                    .entry((regime.name, metric.label.clone(), metric.factor.clone()))
                    .or_default()
                    .push(metric);
            }
        }
    }

    grouped
        .into_iter()
        .filter_map(|((regime_name, label, factor), rows)| {
            if rows.is_empty() {
                return None;
            }
            let total_n = rows.iter().map(|row| row.n).sum::<usize>();
            if total_n == 0 {
                return None;
            }
            let mean_abs_spearman = rows
                .iter()
                .map(|row| row.spearman_ic.abs() * row.n as f64)
                .sum::<f64>()
                / total_n as f64;
            let mean_signed_spearman = rows
                .iter()
                .map(|row| row.spearman_ic * row.n as f64)
                .sum::<f64>()
                / total_n as f64;
            let strongest = rows
                .iter()
                .max_by(|lhs, rhs| lhs.spearman_ic.abs().total_cmp(&rhs.spearman_ic.abs()))?;

            Some(RegimeFactorSummary {
                regime_name,
                label,
                factor,
                mean_abs_spearman,
                mean_signed_spearman,
                total_n,
                strongest_bucket_start_secs: strongest.bucket_start_secs,
                strongest_bucket_end_secs: strongest.bucket_end_secs,
                strongest_bucket_spearman: strongest.spearman_ic,
            })
        })
        .collect()
}

fn three_layer_regime(seconds: i64) -> &'static str {
    if seconds > 270 {
        "early"
    } else if seconds > 60 {
        "middle"
    } else {
        // last 60s: no trade (liquidity too thin, PM price already converged)
        "expiry"
    }
}

fn sign_vote(value: f64) -> i32 {
    if value > 1e-9 {
        1
    } else if value < -1e-9 {
        -1
    } else {
        0
    }
}

fn row_factor_value(row: &FactorObservation, factor: &str) -> Option<f64> {
    let value = match factor {
        "signed_distance_to_beat" => row.signed_distance_to_beat,
        "abs_distance_to_beat" => row.abs_distance_to_beat,
        "drift_10s" => row.drift_10s,
        "drift_30s" => row.drift_30s,
        "flip_age_secs" => row.flip_age_secs,
        "post_flip_drift" => row.post_flip_drift,
        "sigma_horizon" => row.sigma_horizon,
        "fair_prob_up" => row.fair_prob_up,
        "fair_prob_up_clean" => row.fair_prob_up_clean,
        "prob_disagreement" => row.prob_disagreement,
        "implied_sigma_horizon" => row.implied_sigma_horizon,
        "vol_gap" => row.vol_gap,
        "distance_over_sigma" => row.distance_over_sigma,
        "model_prob_up" => row.model_prob_up,
        "model_edge_up" => row.model_edge_up,
        "reward_risk_up" => row.reward_risk_up,
        "reward_risk_down" => row.reward_risk_down,
        "obi" => row.obi,
        "spread_bps" => row.spread_bps,
        "microprice_offset_bps" => row.microprice_offset_bps,
        "bid_depth_near" => row.bid_depth_near,
        "ask_depth_near" => row.ask_depth_near,
        "depth_ratio" => row.depth_ratio,
        "depth_imbalance" => row.depth_imbalance,
        "depth_far_ratio" => row.depth_far_ratio,
        "depth_acceleration" => row.depth_acceleration,
        "obi_10" => row.obi_10,
        "pm_up_ask" => row.pm_up_ask,
        "pm_down_ask" => row.pm_down_ask,
        "pm_lag_secs" => row.pm_lag_secs,
        "cum_obi_delta_5m" => row.cum_obi_delta_5m,
        "cum_depth_delta_5m" => row.cum_depth_delta_5m,
        "cum_mprice_drift_5m" => row.cum_mprice_drift_5m,
        "cum_trade_imbalance_5m" => row.cum_trade_imbalance_5m,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn row_label_value(row: &FactorObservation, label: &str) -> Option<f64> {
    let value = match label {
        "settlement_up" => row.settlement_up,
        "future_up_ask_change_30s" => row.future_up_ask_change_30s?,
        "future_up_ask_change_60s" => row.future_up_ask_change_60s?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn build_time_bucket_metrics(
    rows: &[FactorObservation],
    factors: &[String],
    labels: &[String],
    bin_secs: i64,
    max_secs: i64,
    min_points: usize,
) -> Vec<TimeBucketMetric> {
    let mut grouped: BTreeMap<(String, String, i64), (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    let bin_secs = bin_secs.max(1);

    for row in rows {
        if row.time_remaining_secs < 0 || row.time_remaining_secs > max_secs {
            continue;
        }
        let bucket_start_secs = (row.time_remaining_secs / bin_secs) * bin_secs;

        for factor in factors {
            let Some(x) = row_factor_value(row, factor) else {
                continue;
            };
            for label in labels {
                let Some(y) = row_label_value(row, label) else {
                    continue;
                };
                let entry = grouped
                    .entry((label.clone(), factor.clone(), bucket_start_secs))
                    .or_default();
                entry.0.push(x);
                entry.1.push(y);
            }
        }
    }

    grouped
        .into_iter()
        .filter_map(|((label, factor, bucket_start_secs), (xs, ys))| {
            if xs.len() < min_points {
                return None;
            }
            Some(TimeBucketMetric {
                label,
                factor,
                bucket_start_secs,
                bucket_end_secs: (bucket_start_secs + bin_secs - 1).min(max_secs),
                n: xs.len(),
                pearson_ic: pearson_ic(&xs, &ys),
                spearman_ic: spearman_ic(&xs, &ys),
            })
        })
        .collect()
}

async fn persist_time_bucket_metrics(
    pool: &sqlx::PgPool,
    analysis_scope: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    symbols_csv: &str,
    bin_secs: i64,
    min_points: usize,
    max_windows: i64,
    lob_sample_secs: i32,
    metrics: &[TimeBucketMetric],
) {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS research_time_conditioned_factor_metrics (
            analysis_scope TEXT NOT NULL,
            start_ts TIMESTAMPTZ NOT NULL,
            end_ts TIMESTAMPTZ NOT NULL,
            symbols_csv TEXT NOT NULL,
            label TEXT NOT NULL,
            factor TEXT NOT NULL,
            bucket_start_secs INTEGER NOT NULL,
            bucket_end_secs INTEGER NOT NULL,
            bin_secs INTEGER NOT NULL,
            min_points INTEGER NOT NULL,
            max_windows INTEGER NOT NULL,
            lob_sample_secs INTEGER NOT NULL,
            n INTEGER NOT NULL,
            pearson_ic DOUBLE PRECISION,
            spearman_ic DOUBLE PRECISION,
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            PRIMARY KEY (analysis_scope, label, factor, bucket_start_secs, bucket_end_secs)
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("create research_time_conditioned_factor_metrics table");

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_research_time_conditioned_factor_metrics_lookup
        ON research_time_conditioned_factor_metrics(
            label,
            factor,
            bucket_start_secs,
            bucket_end_secs
        )
        "#,
    )
    .execute(pool)
    .await
    .expect("create research_time_conditioned_factor_metrics index");

    let mut tx = pool
        .begin()
        .await
        .expect("begin time-ic persistence transaction");
    for metric in metrics {
        sqlx::query(
            r#"
            INSERT INTO research_time_conditioned_factor_metrics (
                analysis_scope,
                start_ts,
                end_ts,
                symbols_csv,
                label,
                factor,
                bucket_start_secs,
                bucket_end_secs,
                bin_secs,
                min_points,
                max_windows,
                lob_sample_secs,
                n,
                pearson_ic,
                spearman_ic,
                updated_at
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW()
            )
            ON CONFLICT (analysis_scope, label, factor, bucket_start_secs, bucket_end_secs)
            DO UPDATE SET
                start_ts = EXCLUDED.start_ts,
                end_ts = EXCLUDED.end_ts,
                symbols_csv = EXCLUDED.symbols_csv,
                bin_secs = EXCLUDED.bin_secs,
                min_points = EXCLUDED.min_points,
                max_windows = EXCLUDED.max_windows,
                lob_sample_secs = EXCLUDED.lob_sample_secs,
                n = EXCLUDED.n,
                pearson_ic = EXCLUDED.pearson_ic,
                spearman_ic = EXCLUDED.spearman_ic,
                updated_at = NOW()
            "#,
        )
        .bind(analysis_scope)
        .bind(start)
        .bind(end)
        .bind(symbols_csv)
        .bind(&metric.label)
        .bind(&metric.factor)
        .bind(metric.bucket_start_secs as i32)
        .bind(metric.bucket_end_secs as i32)
        .bind(bin_secs as i32)
        .bind(min_points as i32)
        .bind(max_windows as i32)
        .bind(lob_sample_secs)
        .bind(metric.n as i32)
        .bind(metric.pearson_ic)
        .bind(metric.spearman_ic)
        .execute(&mut *tx)
        .await
        .expect("insert time-conditioned factor metric");
    }
    tx.commit()
        .await
        .expect("commit time-ic persistence transaction");
}

#[cfg(test)]
mod tests {
    use super::{
        EventMetadataRow, build_entry_targets, descending_abs_f64_cmp,
        event_metadata_rows_to_chronology_inputs, parse_entry_target_token,
    };
    use chrono::{TimeZone, Utc};

    #[test]
    fn descending_abs_f64_cmp_handles_non_finite_values() {
        let mut values = vec![0.25, -0.8, f64::NAN, f64::INFINITY, -0.1];
        values.sort_by(|lhs, rhs| descending_abs_f64_cmp(*lhs, *rhs));

        assert_eq!(values[0], -0.8);
        assert!(values[1].is_finite());
        assert!(values[2].is_finite());
        assert!(!values[3].is_finite());
        assert!(!values[4].is_finite());
    }

    #[test]
    fn build_entry_targets_defaults_to_fine_grained_last_minute_grid() {
        let targets = build_entry_targets(None, None);
        let labels: Vec<_> = targets.iter().map(|target| target.label.as_str()).collect();
        // Default: 290s..10s step 10, then 10s..1s step 1, then @last
        assert!(labels.contains(&"@290s"), "should have @290s");
        assert!(labels.contains(&"@10s"), "should have @10s");
        assert!(labels.contains(&"@9s"), "should have @9s");
        assert!(labels.contains(&"@1s"), "should have @1s");
        assert!(labels.contains(&"@last"), "should have @last");
        assert!(labels.len() >= 30, "should have at least 30 targets");

        let sec10 = targets.iter().find(|target| target.seconds == 10).unwrap();
        let sec1 = targets.iter().find(|target| target.seconds == 1).unwrap();

        assert!(sec10.tolerance_secs <= 5);
        assert!(sec1.tolerance_secs <= 2);
    }

    #[test]
    fn parse_entry_target_token_supports_descending_ranges() {
        assert_eq!(parse_entry_target_token("300:296:2"), vec![300, 298, 296]);
        assert_eq!(parse_entry_target_token("5"), vec![5]);
    }

    #[test]
    fn event_metadata_rows_to_chronology_inputs_preserves_observed_event_set() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let event_ids = vec!["evt-b".to_string(), "evt-a".to_string()];
        let rows = vec![
            EventMetadataRow {
                market_slug: "evt-a".to_string(),
                symbol: "BTCUSDT".to_string(),
                start_time: start,
                end_time: start + chrono::Duration::minutes(5),
            },
            EventMetadataRow {
                market_slug: "evt-b".to_string(),
                symbol: "ETHUSDT".to_string(),
                start_time: start + chrono::Duration::minutes(5),
                end_time: start + chrono::Duration::minutes(10),
            },
        ];

        let inputs = event_metadata_rows_to_chronology_inputs(&event_ids, rows);

        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].event_id, "evt-b");
        assert_eq!(inputs[0].symbol, "ETHUSDT");
        assert_eq!(inputs[1].event_id, "evt-a");
        assert_eq!(inputs[1].symbol, "BTCUSDT");
    }

    #[test]
    #[should_panic(expected = "missing canonical pm_market_metadata rows")]
    fn event_metadata_rows_to_chronology_inputs_fails_on_missing_metadata() {
        let event_ids = vec!["evt-missing".to_string()];
        let _ = event_metadata_rows_to_chronology_inputs(&event_ids, Vec::new());
    }

    #[test]
    #[should_panic(expected = "duplicate pm_market_metadata row")]
    fn event_metadata_rows_to_chronology_inputs_fails_on_duplicate_metadata() {
        let start = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let event_ids = vec!["evt-a".to_string()];
        let rows = vec![
            EventMetadataRow {
                market_slug: "evt-a".to_string(),
                symbol: "BTCUSDT".to_string(),
                start_time: start,
                end_time: start + chrono::Duration::minutes(5),
            },
            EventMetadataRow {
                market_slug: "evt-a".to_string(),
                symbol: "BTCUSDT".to_string(),
                start_time: start,
                end_time: start + chrono::Duration::minutes(5),
            },
        ];

        let _ = event_metadata_rows_to_chronology_inputs(&event_ids, rows);
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url").expect("--db-url required");
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
    let symbols_csv = flag_value(&args, "--symbols").unwrap_or_else(|| "BTCUSDT".to_string());
    let symbols: Vec<String> = symbols_csv
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let discover_windows = args.iter().any(|arg| arg == "--discover-valid-5m-windows");
    let max_windows: i64 = flag_value(&args, "--max-windows")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(8);
    let max_quote_age_secs: i64 = flag_value(&args, "--max-quote-age-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(30);
    // Downsample LOB snapshots: keep 1 tick per N seconds (default 5).
    // Reduces JSONB transfer ~Nx with minimal research quality loss.
    let lob_sample_secs: i32 = flag_value(&args, "--lob-sample-secs")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(5);
    let time_ic_factors = csv_values(flag_value(&args, "--time-ic-factors").as_deref());
    let time_ic_labels = {
        let values = csv_values(flag_value(&args, "--time-ic-labels").as_deref());
        if values.is_empty() {
            vec!["settlement_up".to_string()]
        } else {
            values
        }
    };
    let time_ic_bin_secs = flag_value(&args, "--time-ic-bin-secs")
        .map(|raw| {
            raw.parse::<i64>()
                .unwrap_or_else(|_| panic!("invalid time-ic-bin-secs: {raw}"))
        })
        .unwrap_or(1);
    let time_ic_max_secs = flag_value(&args, "--time-ic-max-secs")
        .map(|raw| {
            raw.parse::<i64>()
                .unwrap_or_else(|_| panic!("invalid time-ic-max-secs: {raw}"))
        })
        .unwrap_or(300);
    let time_ic_min_points = flag_value(&args, "--time-ic-min-points")
        .map(|raw| {
            raw.parse::<usize>()
                .unwrap_or_else(|_| panic!("invalid time-ic-min-points: {raw}"))
        })
        .unwrap_or(25);
    let write_time_ic_to_db = args.iter().any(|arg| arg == "--time-ic-write-db");
    let time_ic_scope = flag_value(&args, "--time-ic-scope");
    let three_layer_confirmations_min = flag_value(&args, "--three-layer-confirmations-min")
        .map(|raw| {
            raw.parse::<usize>()
                .unwrap_or_else(|_| panic!("invalid three-layer-confirmations-min: {raw}"))
        })
        .unwrap_or(2);
    let three_layer_reward_risk_min = flag_value(&args, "--three-layer-reward-risk-min")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid three-layer-reward-risk-min: {raw}"))
        })
        .unwrap_or(0.25);
    let three_layer_max_entry_price = flag_value(&args, "--three-layer-max-entry-price")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid three-layer-max-entry-price: {raw}"))
        })
        .unwrap_or(0.65);
    // Minimum liquidity (ask_size in USDC) required to enter a trade.
    // Filters out thin markets where slippage would be severe.
    let three_layer_min_liquidity = flag_value(&args, "--three-layer-min-liquidity")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid three-layer-min-liquidity: {raw}"))
        })
        .unwrap_or(0.0); // default: no filter (backward-compatible)
    // PM quote staleness filters:
    //   max_pm_lag_secs: skip if quote is too old (PM price unreliable)
    //   min_pm_lag_secs: only trade when quote is stale enough to exploit
    //     (spot has moved but PM hasn't updated yet → mispricing window)
    let three_layer_max_pm_lag_secs = flag_value(&args, "--three-layer-max-pm-lag")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid --three-layer-max-pm-lag: {raw}"))
        })
        .unwrap_or(f64::INFINITY); // default: no upper limit
    let three_layer_min_pm_lag_secs = flag_value(&args, "--three-layer-min-pm-lag")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid --three-layer-min-pm-lag: {raw}"))
        })
        .unwrap_or(0.0); // default: no lower limit
    let three_layer_middle_vol_adjust = flag_value(&args, "--three-layer-middle-vol-adjust")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid three-layer-middle-vol-adjust: {raw}"))
        })
        .unwrap_or(0.08);
    let three_layer_late_distance_adjust = flag_value(&args, "--three-layer-late-distance-adjust")
        .map(|raw| {
            raw.parse::<f64>()
                .unwrap_or_else(|_| panic!("invalid three-layer-late-distance-adjust: {raw}"))
        })
        .unwrap_or(0.08);
    let export_parquet: Option<std::path::PathBuf> =
        flag_value(&args, "--export-parquet").map(std::path::PathBuf::from);
    let export_event_dataset: Option<std::path::PathBuf> =
        flag_value(&args, "--export-event-dataset").map(std::path::PathBuf::from);

    eprintln!(
        "loading factor research range {start} -> {end} for {:?}",
        symbols
    );

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await
        .expect("database connection failed");

    let windows = if discover_windows {
        let discovered = discover_valid_windows(&pool, &symbols, start, end, max_windows).await;
        eprintln!("\n=== Discovered Valid 5m Windows ===");
        for window in &discovered {
            eprintln!(
                "{} {} -> {} events={}",
                window.symbol, window.start_time, window.end_time, window.event_count
            );
        }
        discovered
    } else {
        vec![ValidWindowRow {
            symbol: symbols
                .first()
                .cloned()
                .unwrap_or_else(|| "BTCUSDT".to_string()),
            start_time: start,
            end_time: end,
            event_count: 0,
        }]
    };

    // Compute global time range covering all windows for bulk load.
    let global_start = windows.iter().map(|w| w.start_time).min().unwrap_or(start);
    let global_end = windows.iter().map(|w| w.end_time).max().unwrap_or(end);

    // Collect all unique symbols across windows.
    let bulk_symbols: Vec<String> = {
        let mut seen = std::collections::HashSet::new();
        windows
            .iter()
            .filter(|w| seen.insert(w.symbol.clone()))
            .map(|w| w.symbol.clone())
            .collect()
    };

    eprintln!(
        "\nbulk loading {} -> {} for {:?}",
        global_start, global_end, bulk_symbols
    );

    let t0 = std::time::Instant::now();
    let all_updates = load_from_database_with_options(
        &pool,
        &bulk_symbols,
        global_start,
        global_end,
        &HistoricalLoadOptions {
            require_official_settlement: true,
            ..Default::default()
        },
    )
    .await
    .expect("bulk historical load failed");
    eprintln!("load_from_database_with_options: {:?}", t0.elapsed());

    let t1 = std::time::Instant::now();
    let all_lob_snapshots = load_research_lob_snapshots_sampled(
        &pool,
        &bulk_symbols,
        global_start,
        global_end,
        lob_sample_secs,
    )
    .await
    .expect("bulk lob snapshot load failed");
    eprintln!(
        "load_research_lob_snapshots_sampled (sample_secs={}): {:?}",
        lob_sample_secs,
        t1.elapsed()
    );

    eprintln!(
        "bulk loaded {} updates, {} lob snapshots",
        all_updates.len(),
        all_lob_snapshots.len()
    );

    let mut all_metrics = Vec::new();
    let mut all_observations = Vec::new();
    let mut total_observations = 0usize;
    let mut total_event_rows = 0usize;

    for window in &windows {
        // EventDiscovered sort-ts is end_time - window_secs - 1h.
        // Extend the lower bound by that same offset so EventDiscovered items are included.
        let updates_slice_start =
            window.start_time - chrono::Duration::hours(1) - chrono::Duration::seconds(300);
        let updates_slice = slice_by_time(
            &all_updates,
            updates_slice_start,
            window.end_time,
            MarketUpdate::sort_ts,
        );

        let lob_slice = slice_by_time(
            &all_lob_snapshots,
            window.start_time,
            window.end_time,
            |s| s.ts,
        );

        eprintln!(
            "\nwindow {} {} -> {} updates={} lob={}",
            window.symbol,
            window.start_time,
            window.end_time,
            updates_slice.len(),
            lob_slice.len(),
        );

        // Multi-symbol data in the slice is safe: the function maintains per-symbol
        // state internally, so updates from other symbols do not affect this window's factors.
        let observations =
            build_factor_observations_with_lob(&updates_slice, &lob_slice, max_quote_age_secs);
        let event_rows = build_event_summaries(&observations);
        let metrics = factor_metrics(&observations, &event_rows);

        total_observations += observations.len();
        total_event_rows += event_rows.len();
        all_observations.push(observations);
        all_metrics.push(metrics);
    }

    let aggregated = aggregate_factor_metrics(&all_metrics);
    let flat_obs: Vec<FactorObservation> = all_observations
        .iter()
        .flat_map(|rows| rows.iter().cloned())
        .collect();

    if let Some(ref parquet_path) = export_parquet {
        tracing::info!(path = %parquet_path.display(), observations = flat_obs.len(), "Exporting observations to Parquet");
        ploy_research::export_observations_parquet(&flat_obs, parquet_path)
            .expect("Parquet export failed");
        tracing::info!(path = %parquet_path.display(), "Parquet export complete");
    }

    if let Some(ref dataset_dir) = export_event_dataset {
        tracing::info!(path = %dataset_dir.display(), observations = flat_obs.len(), "Exporting event-root dataset");
        let chronology_events = load_event_chronology_inputs(&pool, &flat_obs).await;
        let dataset_request = EventRootDatasetBuildRequest::new(
            &flat_obs,
            chronology_events,
            DatasetSourceWindow {
                start_time: global_start,
                end_time: global_end,
                symbols: bulk_symbols.clone(),
            },
            standard_event_root_dataset_artifacts(),
            Utc::now(),
        );
        let dataset_build =
            build_event_root_dataset(dataset_request).expect("event-root dataset build failed");
        export_event_root_dataset_parquet(&dataset_build, dataset_dir)
            .expect("event-root dataset export failed");
        eprintln!(
            "\n=== Event Dataset Export ===\npath={}\nevents={} observations={}\nevents_by_split train={} val={} test={}\nobservations_by_split train={} val={} test={}",
            dataset_dir.display(),
            dataset_build.manifest.stats.total_events,
            dataset_build.manifest.stats.total_observations,
            dataset_build.manifest.stats.events_per_split.train,
            dataset_build.manifest.stats.events_per_split.val,
            dataset_build.manifest.stats.events_per_split.test,
            dataset_build.manifest.stats.observations_per_split.train,
            dataset_build.manifest.stats.observations_per_split.val,
            dataset_build.manifest.stats.observations_per_split.test,
        );
        tracing::info!(path = %dataset_dir.display(), "Event-root dataset export complete");
    }

    if !time_ic_factors.is_empty() {
        let mut time_bucket_metrics = build_time_bucket_metrics(
            &flat_obs,
            &time_ic_factors,
            &time_ic_labels,
            time_ic_bin_secs,
            time_ic_max_secs,
            time_ic_min_points,
        );
        time_bucket_metrics.sort_by(|lhs, rhs| {
            lhs.label
                .cmp(&rhs.label)
                .then(lhs.factor.cmp(&rhs.factor))
                .then(rhs.bucket_start_secs.cmp(&lhs.bucket_start_secs))
        });

        if write_time_ic_to_db {
            let analysis_scope = time_ic_scope.clone().unwrap_or_else(|| {
                format!(
                    "factor-research:{}:{}:{}:bin{}:min{}:windows{}:lob{}",
                    start.to_rfc3339(),
                    end.to_rfc3339(),
                    symbols.join(","),
                    time_ic_bin_secs,
                    time_ic_min_points,
                    max_windows,
                    lob_sample_secs
                )
            });
            persist_time_bucket_metrics(
                &pool,
                &analysis_scope,
                start,
                end,
                &symbols.join(","),
                time_ic_bin_secs,
                time_ic_min_points,
                max_windows,
                lob_sample_secs,
                &time_bucket_metrics,
            )
            .await;
            eprintln!(
                "time_conditioned_ic_persisted scope={} rows={}",
                analysis_scope,
                time_bucket_metrics.len()
            );
            let persisted_rows: i64 = sqlx::query_scalar(
                r#"
                SELECT COUNT(*)
                FROM research_time_conditioned_factor_metrics
                WHERE analysis_scope = $1
                "#,
            )
            .bind(&analysis_scope)
            .fetch_one(&pool)
            .await
            .expect("count persisted time-conditioned ic rows");
            let strongest_row: Option<(String, String, i32, i32, i32, f64)> = sqlx::query_as(
                r#"
                SELECT factor, label, bucket_start_secs, bucket_end_secs, n, ABS(COALESCE(spearman_ic, 0.0)) AS abs_spearman
                FROM research_time_conditioned_factor_metrics
                WHERE analysis_scope = $1
                ORDER BY abs_spearman DESC, factor ASC, label ASC
                LIMIT 1
                "#,
            )
            .bind(&analysis_scope)
            .fetch_optional(&pool)
            .await
            .expect("load strongest persisted time-conditioned ic row");
            if let Some((factor, label, bucket_start_secs, bucket_end_secs, n, abs_spearman)) =
                strongest_row
            {
                eprintln!(
                    "time_conditioned_ic_db_verify scope={} persisted_rows={} strongest={} {} {}..{}s n={} abs_spearman={:>7.4}",
                    analysis_scope,
                    persisted_rows,
                    factor,
                    label,
                    bucket_start_secs,
                    bucket_end_secs,
                    n,
                    abs_spearman
                );
            } else {
                eprintln!(
                    "time_conditioned_ic_db_verify scope={} persisted_rows={} strongest=none",
                    analysis_scope, persisted_rows
                );
            }
        }

        eprintln!(
            "\n=== Time-Conditioned IC (bin={}s, max={}s, min_points={}) ===",
            time_ic_bin_secs, time_ic_max_secs, time_ic_min_points
        );
        for metric in &time_bucket_metrics {
            eprintln!(
                "time_ic label={} factor={} bucket={}..{}s n={} pearson={:>7.4} spearman={:>7.4}",
                metric.label,
                metric.factor,
                metric.bucket_start_secs,
                metric.bucket_end_secs,
                metric.n,
                metric.pearson_ic,
                metric.spearman_ic
            );
        }

        eprintln!("\n=== Time-Conditioned IC Summary ===");
        for label in &time_ic_labels {
            for factor in &time_ic_factors {
                let factor_rows: Vec<&TimeBucketMetric> = time_bucket_metrics
                    .iter()
                    .filter(|metric| &metric.label == label && &metric.factor == factor)
                    .collect();
                if factor_rows.is_empty() {
                    continue;
                }
                if let Some(best) = factor_rows
                    .iter()
                    .filter(|metric| metric.spearman_ic.is_finite())
                    .max_by(|lhs, rhs| lhs.spearman_ic.abs().total_cmp(&rhs.spearman_ic.abs()))
                    .copied()
                {
                    let last = factor_rows
                        .iter()
                        .find(|metric| metric.bucket_start_secs == 0)
                        .copied();
                    eprintln!(
                        "{:<24} label={:<24} strongest_bucket={}..{}s n={} spearman={:>7.4}{}",
                        factor,
                        label,
                        best.bucket_start_secs,
                        best.bucket_end_secs,
                        best.n,
                        best.spearman_ic,
                        last.map(|metric| format!(
                            "  last_bucket_spearman={:>7.4}",
                            metric.spearman_ic
                        ))
                        .unwrap_or_default()
                    );
                }
            }
        }

        let regimes = default_time_regimes(time_ic_max_secs);
        let mut regime_summaries = summarize_regime_factors(&time_bucket_metrics, &regimes);
        regime_summaries.sort_by(|lhs, rhs| {
            lhs.label
                .cmp(&rhs.label)
                .then(lhs.regime_name.cmp(rhs.regime_name))
                .then(rhs.mean_abs_spearman.total_cmp(&lhs.mean_abs_spearman))
        });

        eprintln!("\n=== Regime Factor Summary ===");
        for label in &time_ic_labels {
            for regime in &regimes {
                let top_rows: Vec<&RegimeFactorSummary> = regime_summaries
                    .iter()
                    .filter(|row| &row.label == label && row.regime_name == regime.name)
                    .take(3)
                    .collect();
                if top_rows.is_empty() {
                    continue;
                }
                eprintln!(
                    "regime label={} regime={} window={}..{}s",
                    label, regime.name, regime.start_secs, regime.end_secs
                );
                for row in top_rows {
                    eprintln!(
                        "  factor={:<22} mean_abs_spearman={:>7.4} mean_signed={:>7.4} total_n={} strongest_bucket={}..{}s strongest_spearman={:>7.4}",
                        row.factor,
                        row.mean_abs_spearman,
                        row.mean_signed_spearman,
                        row.total_n,
                        row.strongest_bucket_start_secs,
                        row.strongest_bucket_end_secs,
                        row.strongest_bucket_spearman,
                    );
                }
            }
        }
    }

    eprintln!("\nobservation_rows={}", total_observations);
    eprintln!("event_rows={}", total_event_rows);

    // === P&L Simulation (6 strategy variants, expanding-window calibration) ===
    // For each event, simulate a trade at the last observation before settlement.
    // Exit: hold to settlement ($1 win, $0 loss)
    //
    // Variants:
    //   A. Baseline    – original model direction (model_prob_up > pm_up_ask + fee)
    //   B. Contrarian  – flip model direction (IC=-0.27 suggests model is systematically wrong)
    //   C. LOB-only    – trade only when obi_10 + depth_imbalance agree on direction
    //   D. Combined    – contrarian model + LOB direction filter must agree
    //   E. Calibrated  – empirical P(up|d/σ) from expanding window replaces log-normal CDF
    //   F. Cal+LOB     – calibrated probability + LOB direction filter must agree
    //   G. MultiFactor – IC-weighted multi-factor composite → sigmoid → probability
    //   H. MF+LOB     – multi-factor composite + LOB direction filter must agree

    #[derive(Default)]
    struct SimStats {
        trades: u32,
        wins: u32,
        pnl: f64,
        stake: f64,
        priced_trades: u32,
        entry_price_sum: f64,
        reward_risk_sum: f64,
        pnl_series: Vec<f64>, // per-trade P&L for Monte Carlo
    }

    impl SimStats {
        fn record(&mut self, won: bool, pnl: f64, stake: f64, entry_price: f64, reward_risk: f64) {
            self.trades += 1;
            if won {
                self.wins += 1;
            }
            self.pnl += pnl;
            self.stake += stake;
            self.pnl_series.push(pnl);
            if entry_price.is_finite() {
                self.priced_trades += 1;
                self.entry_price_sum += entry_price;
            }
            if reward_risk.is_finite() {
                self.reward_risk_sum += reward_risk;
            }
        }
        fn win_rate(&self) -> f64 {
            if self.trades > 0 {
                self.wins as f64 / self.trades as f64 * 100.0
            } else {
                0.0
            }
        }
        fn roi(&self) -> f64 {
            if self.stake > 0.0 {
                self.pnl / self.stake * 100.0
            } else {
                0.0
            }
        }
        fn avg_entry_price(&self) -> f64 {
            if self.priced_trades > 0 {
                self.entry_price_sum / self.priced_trades as f64
            } else {
                f64::NAN
            }
        }
        fn avg_reward_risk(&self) -> f64 {
            if self.trades > 0 {
                self.reward_risk_sum / self.trades as f64
            } else {
                f64::NAN
            }
        }
    }

    // --- Multi-factor IC-weighted model ---
    // Factors: obi_10, depth_imbalance, depth_acceleration, spread_bps,
    //          cum_obi_delta_5m, cum_depth_delta_5m, cum_mprice_drift_5m,
    //          cum_trade_imbalance_5m, drift_10s, drift_30s, pm_lag_secs
    const MF_N: usize = 11;

    fn extract_factors(obs: &ploy_research::FactorObservation) -> [f64; MF_N] {
        [
            obs.obi_10,
            obs.depth_imbalance,
            obs.depth_acceleration,
            obs.spread_bps,
            obs.cum_obi_delta_5m,
            obs.cum_depth_delta_5m,
            obs.cum_mprice_drift_5m,
            obs.cum_trade_imbalance_5m,
            obs.drift_10s,
            obs.drift_30s,
            obs.pm_lag_secs,
        ]
    }

    struct MultiFactorModel {
        // Accumulated: (factor_values, settlement_up)
        data: Vec<([f64; MF_N], f64)>,
    }

    impl MultiFactorModel {
        fn new() -> Self {
            Self { data: Vec::new() }
        }

        fn push(&mut self, factors: [f64; MF_N], settlement: f64) {
            if factors.iter().all(|f| f.is_finite()) && (settlement == 0.0 || settlement == 1.0) {
                self.data.push((factors, settlement));
            }
        }

        /// Compute IC weights and factor statistics from accumulated data.
        /// Returns (weights, means, stds) or None if insufficient data.
        fn fit(&self) -> Option<([f64; MF_N], [f64; MF_N], [f64; MF_N])> {
            let n = self.data.len();
            if n < 100 {
                return None;
            }

            let mut means = [0.0f64; MF_N];
            let mut y_mean = 0.0f64;
            for (x, y) in &self.data {
                for i in 0..MF_N {
                    means[i] += x[i];
                }
                y_mean += y;
            }
            for i in 0..MF_N {
                means[i] /= n as f64;
            }
            y_mean /= n as f64;

            let mut stds = [0.0f64; MF_N];
            let mut y_var = 0.0f64;
            let mut cov = [0.0f64; MF_N];
            for (x, y) in &self.data {
                let dy = y - y_mean;
                y_var += dy * dy;
                for i in 0..MF_N {
                    let dx = x[i] - means[i];
                    stds[i] += dx * dx;
                    cov[i] += dx * dy;
                }
            }
            for i in 0..MF_N {
                stds[i] = (stds[i] / n as f64).sqrt();
            }
            y_var = (y_var / n as f64).sqrt();

            // Pearson correlation as weight
            let mut weights = [0.0f64; MF_N];
            for i in 0..MF_N {
                if stds[i] > 1e-12 && y_var > 1e-12 {
                    weights[i] = cov[i] / (n as f64 * stds[i] * y_var);
                }
            }

            Some((weights, means, stds))
        }

        /// Predict P(up) for a new observation using IC-weighted z-scores + sigmoid.
        fn predict(
            &self,
            factors: &[f64; MF_N],
            weights: &[f64; MF_N],
            means: &[f64; MF_N],
            stds: &[f64; MF_N],
        ) -> f64 {
            let mut composite = 0.0f64;
            for i in 0..MF_N {
                if stds[i] > 1e-12 && factors[i].is_finite() {
                    let z = (factors[i] - means[i]) / stds[i];
                    composite += weights[i] * z;
                }
            }
            // Sigmoid: map composite to [0, 1]
            // Scale factor 2.0 so that composite=±1 maps to ~0.88/0.12
            1.0 / (1.0 + (-2.0 * composite).exp())
        }
    }

    // Entry time targets: test entering at different points in the 5-minute window.
    // Use a denser hybrid grid in the final minute by default.
    let entry_targets_raw = flag_value(&args, "--entry-targets-secs");
    let entry_tolerance_secs = flag_value(&args, "--entry-tolerance-secs").map(|raw| {
        raw.parse::<i64>()
            .unwrap_or_else(|_| panic!("invalid tolerance: {raw}"))
    });
    let entry_targets = build_entry_targets(entry_targets_raw.as_deref(), entry_tolerance_secs);

    let stake_per_trade = 25.0f64;
    let min_edge = 0.02f64;

    // Expanding-window accumulators (shared across entry targets)
    let mut cal_data: Vec<(f64, f64)> = Vec::new();
    let mut mf_model = MultiFactorModel::new();

    // Per-entry-time stats: (label, D.Combined, G.MultiFactor, H.MF+LOB, I.ThreeLayer)
    struct EntryStats {
        total_events: u32,
        covered_events: u32,
        combined: SimStats,
        mf: SimStats,
        mf_lob: SimStats,
        three_layer: SimStats,
    }
    let mut entry_results: Vec<(String, EntryStats)> = entry_targets
        .iter()
        .map(|target| {
            (
                target.label.clone(),
                EntryStats {
                    total_events: 0,
                    covered_events: 0,
                    combined: SimStats::default(),
                    mf: SimStats::default(),
                    mf_lob: SimStats::default(),
                    three_layer: SimStats::default(),
                },
            )
        })
        .collect();

    #[derive(Default)]
    struct OneTradeStats {
        overall: SimStats,
        early: SimStats,
        middle: SimStats,
        late: SimStats,
        expiry: SimStats,
        skipped_events: u32,
    }
    let mut three_layer_one_trade = OneTradeStats::default();

    for observations in &all_observations {
        let mf_fit = mf_model.fit();

        // Group observations by event_id
        let mut by_event: std::collections::HashMap<&str, Vec<&ploy_research::FactorObservation>> =
            std::collections::HashMap::new();
        for obs in observations.iter() {
            by_event.entry(obs.event_id.as_str()).or_default().push(obs);
        }
        for event_obs in by_event.values_mut() {
            event_obs.sort_by_key(|obs| obs.tick_ts);
        }

        // For each entry time target, find the best observation per event
        for (target_idx, target) in entry_targets.iter().enumerate() {
            for event_obs in by_event.values() {
                let stats = &mut entry_results[target_idx].1;
                stats.total_events += 1;

                // Find observation closest to target time_remaining
                // For @last (target=0), take the last observation
                let obs = if target.seconds == 0 {
                    event_obs.iter().max_by_key(|o| o.tick_ts)
                } else {
                    // Find obs with time_remaining closest to target
                    event_obs
                        .iter()
                        .min_by_key(|o| (o.time_remaining_secs - target.seconds).abs())
                };
                let Some(obs) = obs else { continue };

                // Skip if too far from target based on the configured or derived tolerance.
                if target.seconds > 0
                    && (obs.time_remaining_secs - target.seconds).abs() > target.tolerance_secs
                {
                    continue;
                }
                stats.covered_events += 1;

                if !obs.pm_up_ask.is_finite()
                    || !obs.pm_down_ask.is_finite()
                    || !obs.model_prob_up.is_finite()
                {
                    continue;
                }

                let fee_up = 0.02 * obs.pm_up_ask * (1.0 - obs.pm_up_ask);
                let fee_down = 0.02 * obs.pm_down_ask * (1.0 - obs.pm_down_ask);

                let pnl_up = |won: bool| -> (f64, bool) {
                    let p = if won {
                        stake_per_trade * (1.0 / obs.pm_up_ask - 1.0)
                            - stake_per_trade * fee_up / obs.pm_up_ask
                    } else {
                        -stake_per_trade - stake_per_trade * fee_up / obs.pm_up_ask
                    };
                    (p, won)
                };
                let pnl_down = |won: bool| -> (f64, bool) {
                    let p = if won {
                        stake_per_trade * (1.0 / obs.pm_down_ask - 1.0)
                            - stake_per_trade * fee_down / obs.pm_down_ask
                    } else {
                        -stake_per_trade - stake_per_trade * fee_down / obs.pm_down_ask
                    };
                    (p, won)
                };

                // --- D. Combined (contrarian + LOB) ---
                {
                    let cp = 1.0 - obs.model_prob_up;
                    let edge_up = cp - obs.pm_up_ask - fee_up;
                    let edge_down = (1.0 - cp) - obs.pm_down_ask - fee_down;
                    let lob_score =
                        obs.obi_10 + obs.depth_imbalance - 0.5 * obs.microprice_offset_bps.signum();
                    if edge_up >= min_edge && edge_up >= edge_down && lob_score > 0.0 {
                        let (p, w) = pnl_up(obs.settlement_up == 1.0);
                        stats.combined.record(
                            w,
                            p,
                            stake_per_trade,
                            obs.pm_up_ask,
                            obs.reward_risk_up,
                        );
                    } else if edge_down >= min_edge && lob_score < 0.0 {
                        let (p, w) = pnl_down(obs.settlement_up == 0.0);
                        stats.combined.record(
                            w,
                            p,
                            stake_per_trade,
                            obs.pm_down_ask,
                            obs.reward_risk_down,
                        );
                    }
                }

                // --- G. MultiFactor ---
                if let Some((ref weights, ref means, ref stds)) = mf_fit {
                    let fv = extract_factors(obs);
                    let mf_p = mf_model.predict(&fv, weights, means, stds);
                    let edge_up = mf_p - obs.pm_up_ask - fee_up;
                    let edge_down = (1.0 - mf_p) - obs.pm_down_ask - fee_down;
                    if edge_up >= min_edge && edge_up >= edge_down {
                        let (p, w) = pnl_up(obs.settlement_up == 1.0);
                        stats
                            .mf
                            .record(w, p, stake_per_trade, obs.pm_up_ask, obs.reward_risk_up);
                    } else if edge_down >= min_edge {
                        let (p, w) = pnl_down(obs.settlement_up == 0.0);
                        stats.mf.record(
                            w,
                            p,
                            stake_per_trade,
                            obs.pm_down_ask,
                            obs.reward_risk_down,
                        );
                    }
                }

                // --- H. MF+LOB ---
                if let Some((ref weights, ref means, ref stds)) = mf_fit {
                    let fv = extract_factors(obs);
                    let mf_p = mf_model.predict(&fv, weights, means, stds);
                    let edge_up = mf_p - obs.pm_up_ask - fee_up;
                    let edge_down = (1.0 - mf_p) - obs.pm_down_ask - fee_down;
                    let lob_score =
                        obs.obi_10 + obs.depth_imbalance - 0.5 * obs.microprice_offset_bps.signum();
                    if edge_up >= min_edge && edge_up >= edge_down && lob_score > 0.0 {
                        let (p, w) = pnl_up(obs.settlement_up == 1.0);
                        stats.mf_lob.record(
                            w,
                            p,
                            stake_per_trade,
                            obs.pm_up_ask,
                            obs.reward_risk_up,
                        );
                    } else if edge_down >= min_edge && lob_score < 0.0 {
                        let (p, w) = pnl_down(obs.settlement_up == 0.0);
                        stats.mf_lob.record(
                            w,
                            p,
                            stake_per_trade,
                            obs.pm_down_ask,
                            obs.reward_risk_down,
                        );
                    }
                }

                // --- I. ThreeLayer ---
                {
                    let regime = if target.seconds > 270 {
                        "early"
                    } else if target.seconds > 60 {
                        "middle"
                    } else {
                        "expiry"
                    };

                    if regime != "expiry"
                        && obs.fair_prob_up_clean.is_finite()
                        && obs.model_prob_up.is_finite()
                        && obs.reward_risk_up.is_finite()
                        && obs.reward_risk_down.is_finite()
                    {
                        let independent_state_up =
                            (1.0 - obs.model_prob_up).clamp(1e-4, 1.0 - 1e-4);
                        let p_three_up = match regime {
                            "early" => independent_state_up,
                            "middle" => {
                                let vol_adjust = if obs.vol_gap.is_finite() {
                                    three_layer_middle_vol_adjust * sign_vote(obs.vol_gap) as f64
                                } else {
                                    0.0
                                };
                                (0.65 * independent_state_up
                                    + 0.35 * obs.fair_prob_up_clean
                                    + vol_adjust)
                                    .clamp(1e-4, 1.0 - 1e-4)
                            }
                            _ => 0.5,
                        };

                        let mut direction: Option<bool> = None; // true = UP, false = DOWN
                        if p_three_up > 0.5 {
                            direction = Some(true);
                        } else if p_three_up < 0.5 {
                            direction = Some(false);
                        }

                        let direction = direction.filter(|side_up| match regime {
                            "early" => true,
                            "middle" => {
                                if obs.vol_gap.is_finite() {
                                    (*side_up && obs.vol_gap > 0.0)
                                        || (!*side_up && obs.vol_gap < 0.0)
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        });

                        if let Some(side_up) = direction {
                            let lob_votes = [
                                sign_vote(obs.drift_30s),
                                sign_vote(obs.obi_10),
                                sign_vote(obs.depth_imbalance),
                                sign_vote(obs.cum_mprice_drift_5m),
                            ];
                            let desired_vote = if side_up { 1 } else { -1 };
                            let confirmations = lob_votes
                                .iter()
                                .filter(|vote| **vote == desired_vote)
                                .count();

                            if side_up {
                                let edge = p_three_up - (obs.pm_up_ask + fee_up);
                                if confirmations >= three_layer_confirmations_min
                                    && edge >= min_edge
                                    && obs.pm_up_ask <= three_layer_max_entry_price
                                    && obs.reward_risk_up >= three_layer_reward_risk_min
                                {
                                    let (p, w) = pnl_up(obs.settlement_up == 1.0);
                                    stats.three_layer.record(
                                        w,
                                        p,
                                        stake_per_trade,
                                        obs.pm_up_ask,
                                        obs.reward_risk_up,
                                    );
                                }
                            } else {
                                let edge = (1.0 - p_three_up) - (obs.pm_down_ask + fee_down);
                                if confirmations >= three_layer_confirmations_min
                                    && edge >= min_edge
                                    && obs.pm_down_ask <= three_layer_max_entry_price
                                    && obs.reward_risk_down >= three_layer_reward_risk_min
                                {
                                    let (p, w) = pnl_down(obs.settlement_up == 0.0);
                                    stats.three_layer.record(
                                        w,
                                        p,
                                        stake_per_trade,
                                        obs.pm_down_ask,
                                        obs.reward_risk_down,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // Real strategy accounting: at most one trade per event, enter on the
        // first qualifying ThreeLayer signal and hold to settlement.
        for event_obs in by_event.values() {
            let mut traded = false;
            for obs in event_obs.iter() {
                if !obs.pm_up_ask.is_finite()
                    || !obs.pm_down_ask.is_finite()
                    || !obs.model_prob_up.is_finite()
                    || !obs.fair_prob_up_clean.is_finite()
                    || !obs.reward_risk_up.is_finite()
                    || !obs.reward_risk_down.is_finite()
                {
                    continue;
                }

                let regime = three_layer_regime(obs.time_remaining_secs);
                if regime == "expiry" {
                    continue;
                }

                let fee_up = 0.02 * obs.pm_up_ask * (1.0 - obs.pm_up_ask);
                let fee_down = 0.02 * obs.pm_down_ask * (1.0 - obs.pm_down_ask);

                let independent_state_up = (1.0 - obs.model_prob_up).clamp(1e-4, 1.0 - 1e-4);
                let p_three_up = match regime {
                    "early" => independent_state_up,
                    "middle" => {
                        let vol_adjust = if obs.vol_gap.is_finite() {
                            three_layer_middle_vol_adjust * sign_vote(obs.vol_gap) as f64
                        } else {
                            0.0
                        };
                        (0.65 * independent_state_up + 0.35 * obs.fair_prob_up_clean + vol_adjust)
                            .clamp(1e-4, 1.0 - 1e-4)
                    }
                    _ => 0.5,
                };

                let side_up = if p_three_up > 0.5 {
                    Some(true)
                } else if p_three_up < 0.5 {
                    Some(false)
                } else {
                    None
                };

                let side_up = side_up.filter(|side_up| match regime {
                    "early" => true,
                    "middle" => {
                        if obs.vol_gap.is_finite() {
                            (*side_up && obs.vol_gap > 0.0) || (!*side_up && obs.vol_gap < 0.0)
                        } else {
                            false
                        }
                    }
                    _ => false,
                });

                let Some(side_up) = side_up else { continue };

                let lob_votes = [
                    sign_vote(obs.drift_30s),
                    sign_vote(obs.obi_10),
                    sign_vote(obs.depth_imbalance),
                    sign_vote(obs.cum_mprice_drift_5m),
                ];
                let desired_vote = if side_up { 1 } else { -1 };
                let confirmations = lob_votes
                    .iter()
                    .filter(|vote| **vote == desired_vote)
                    .count();
                if confirmations < three_layer_confirmations_min {
                    continue;
                }

                // Pre-fill edge check (at quoted price, before impact adjustment)
                let (pre_fill_edge, pre_fill_rr) = if side_up {
                    (p_three_up - (obs.pm_up_ask + fee_up), obs.reward_risk_up)
                } else {
                    (
                        (1.0 - p_three_up) - (obs.pm_down_ask + fee_down),
                        obs.reward_risk_down,
                    )
                };

                let entry_price = if side_up {
                    obs.pm_up_ask
                } else {
                    obs.pm_down_ask
                };

                if pre_fill_edge < min_edge || pre_fill_rr < three_layer_reward_risk_min {
                    continue;
                }
                if entry_price > three_layer_max_entry_price {
                    continue;
                }

                // PM quote staleness filter:
                // - Skip if quote is too stale (PM price unreliable, direction unknown)
                // - Skip if quote is too fresh (no mispricing window yet)
                if obs.pm_lag_secs.is_finite() {
                    if obs.pm_lag_secs > three_layer_max_pm_lag_secs {
                        continue; // quote too old, PM price unreliable
                    }
                    if obs.pm_lag_secs < three_layer_min_pm_lag_secs {
                        continue; // quote too fresh, no stale-quote edge yet
                    }
                }

                // Fill simulation using orderbook depth.
                // order_shares = how many shares we need to buy at entry_price
                let order_shares = stake_per_trade / entry_price;
                let ask_size = if side_up {
                    obs.pm_up_ask_size
                } else {
                    obs.pm_down_ask_size
                };
                let bid_size = if side_up {
                    obs.pm_up_bid_size
                } else {
                    obs.pm_down_bid_size
                };

                // Liquidity filter: skip if ask_size is known and below threshold
                if three_layer_min_liquidity > 0.0
                    && ask_size.is_finite()
                    && ask_size < three_layer_min_liquidity
                {
                    continue;
                }

                // Price impact: if best ask can't fill our full order, we walk up one tick.
                // This is conservative — real impact could be larger for thin books.
                let fill_price =
                    if ask_size.is_finite() && ask_size > 0.0 && ask_size < order_shares {
                        (entry_price + 0.01).min(0.99) // one tick slippage
                    } else {
                        entry_price
                    };

                // Recalculate edge and pnl with actual fill price
                let fill_fee = 0.02 * fill_price * (1.0 - fill_price);
                let (edge, reward_risk, won, pnl) = if side_up {
                    let e = p_three_up - (fill_price + fill_fee);
                    let rr = if fill_price > 0.0 {
                        (1.0 / fill_price) - 1.0
                    } else {
                        0.0
                    };
                    let p = if obs.settlement_up == 1.0 {
                        stake_per_trade * (1.0 / fill_price - 1.0)
                            - stake_per_trade * fill_fee / fill_price
                    } else {
                        -stake_per_trade - stake_per_trade * fill_fee / fill_price
                    };
                    (e, rr, obs.settlement_up == 1.0, p)
                } else {
                    let e = (1.0 - p_three_up) - (fill_price + fill_fee);
                    let rr = if fill_price > 0.0 {
                        (1.0 / fill_price) - 1.0
                    } else {
                        0.0
                    };
                    let p = if obs.settlement_up == 0.0 {
                        stake_per_trade * (1.0 / fill_price - 1.0)
                            - stake_per_trade * fill_fee / fill_price
                    } else {
                        -stake_per_trade - stake_per_trade * fill_fee / fill_price
                    };
                    (e, rr, obs.settlement_up == 0.0, p)
                };
                let _ = bid_size; // available for future spread-based filters

                traded = true;
                three_layer_one_trade.overall.record(
                    won,
                    pnl,
                    stake_per_trade,
                    fill_price,
                    reward_risk,
                );
                match regime {
                    "early" => three_layer_one_trade.early.record(
                        won,
                        pnl,
                        stake_per_trade,
                        fill_price,
                        reward_risk,
                    ),
                    "middle" => three_layer_one_trade.middle.record(
                        won,
                        pnl,
                        stake_per_trade,
                        fill_price,
                        reward_risk,
                    ),
                    "late" => three_layer_one_trade.late.record(
                        won,
                        pnl,
                        stake_per_trade,
                        fill_price,
                        reward_risk,
                    ),
                    _ => three_layer_one_trade.expiry.record(
                        won,
                        pnl,
                        stake_per_trade,
                        fill_price,
                        reward_risk,
                    ),
                }
                break;
            }
            if !traded {
                three_layer_one_trade.skipped_events += 1;
            }
        }

        // Accumulate for expanding-window models
        for obs in observations.iter() {
            if obs.distance_over_sigma.is_finite()
                && (obs.settlement_up == 0.0 || obs.settlement_up == 1.0)
            {
                cal_data.push((obs.distance_over_sigma, obs.settlement_up));
            }
            mf_model.push(extract_factors(obs), obs.settlement_up);
        }
    }

    eprintln!(
        "\n=== P&L by Entry Time (min_edge=2%, stake=$25, three_layer_confirm_min={}, three_layer_rr_min={:.2}, three_layer_max_entry={:.2}, min_liquidity={:.0}, middle_vol_adjust={:.2}) ===",
        three_layer_confirmations_min,
        three_layer_reward_risk_min,
        three_layer_max_entry_price,
        three_layer_min_liquidity,
        three_layer_middle_vol_adjust
    );
    eprintln!(
        "{:<8} {:<14} {:<43} {:<43} {:<43} {:<43}",
        "entry", "coverage", "D.Combined", "G.MultiFactor", "H.MF+LOB", "I.ThreeLayer"
    );
    for (label, stats) in &entry_results {
        eprintln!(
            "{:<8} {:<14} t={:<4} w={:<4} wr={:>5.1}% roi={:>7.2}% | t={:<4} w={:<4} wr={:>5.1}% roi={:>7.2}% | t={:<4} w={:<4} wr={:>5.1}% roi={:>7.2}% | t={:<4} w={:<4} wr={:>5.1}% roi={:>7.2}%",
            label,
            format!("{}/{}", stats.covered_events, stats.total_events),
            stats.combined.trades,
            stats.combined.wins,
            stats.combined.win_rate(),
            stats.combined.roi(),
            stats.mf.trades,
            stats.mf.wins,
            stats.mf.win_rate(),
            stats.mf.roi(),
            stats.mf_lob.trades,
            stats.mf_lob.wins,
            stats.mf_lob.win_rate(),
            stats.mf_lob.roi(),
            stats.three_layer.trades,
            stats.three_layer.wins,
            stats.three_layer.win_rate(),
            stats.three_layer.roi(),
        );
    }

    eprintln!("\n=== I.ThreeLayer One-Event-One-Trade ===");
    eprintln!(
        "overall  trades={} wins={} win_rate={:>5.1}% roi={:>7.2}% avg_entry={:>6.3} avg_rr={:>6.3} skipped_events={}",
        three_layer_one_trade.overall.trades,
        three_layer_one_trade.overall.wins,
        three_layer_one_trade.overall.win_rate(),
        three_layer_one_trade.overall.roi(),
        three_layer_one_trade.overall.avg_entry_price(),
        three_layer_one_trade.overall.avg_reward_risk(),
        three_layer_one_trade.skipped_events,
    );
    eprintln!(
        "early    trades={} wins={} win_rate={:>5.1}% roi={:>7.2}% avg_entry={:>6.3} avg_rr={:>6.3}",
        three_layer_one_trade.early.trades,
        three_layer_one_trade.early.wins,
        three_layer_one_trade.early.win_rate(),
        three_layer_one_trade.early.roi(),
        three_layer_one_trade.early.avg_entry_price(),
        three_layer_one_trade.early.avg_reward_risk(),
    );
    eprintln!(
        "middle   trades={} wins={} win_rate={:>5.1}% roi={:>7.2}% avg_entry={:>6.3} avg_rr={:>6.3}",
        three_layer_one_trade.middle.trades,
        three_layer_one_trade.middle.wins,
        three_layer_one_trade.middle.win_rate(),
        three_layer_one_trade.middle.roi(),
        three_layer_one_trade.middle.avg_entry_price(),
        three_layer_one_trade.middle.avg_reward_risk(),
    );
    eprintln!(
        "late     trades={} wins={} win_rate={:>5.1}% roi={:>7.2}% avg_entry={:>6.3} avg_rr={:>6.3}",
        three_layer_one_trade.late.trades,
        three_layer_one_trade.late.wins,
        three_layer_one_trade.late.win_rate(),
        three_layer_one_trade.late.roi(),
        three_layer_one_trade.late.avg_entry_price(),
        three_layer_one_trade.late.avg_reward_risk(),
    );

    // === Monte Carlo Analysis ===
    {
        let series = &three_layer_one_trade.overall.pnl_series;
        let n = series.len();
        if n >= 10 {
            let stake = stake_per_trade;
            let bankroll_start = 500.0f64; // user's starting capital in USDC

            // --- Actual series metrics ---
            let mean_pnl = series.iter().sum::<f64>() / n as f64;
            let variance = series.iter().map(|x| (x - mean_pnl).powi(2)).sum::<f64>() / n as f64;
            let std_pnl = variance.sqrt();
            let downside_variance = series
                .iter()
                .filter(|&&x| x < 0.0)
                .map(|x| x.powi(2))
                .sum::<f64>()
                / n as f64;
            let downside_std = downside_variance.sqrt();

            // Sharpe (per-trade, annualized assuming ~6 trades/day × 365)
            let trades_per_year = 6.0_f64 * 365.0;
            let sharpe = if std_pnl > 0.0 {
                (mean_pnl / std_pnl) * trades_per_year.sqrt()
            } else {
                0.0
            };
            let sortino = if downside_std > 0.0 {
                (mean_pnl / downside_std) * trades_per_year.sqrt()
            } else {
                0.0
            };

            // Max drawdown on actual series
            let mut peak = 0.0f64;
            let mut equity = 0.0f64;
            let mut max_dd = 0.0f64;
            for &p in series {
                equity += p;
                if equity > peak {
                    peak = equity;
                }
                let dd = peak - equity;
                if dd > max_dd {
                    max_dd = dd;
                }
            }
            let max_dd_pct = if peak > 0.0 {
                max_dd / (bankroll_start + peak) * 100.0
            } else {
                0.0
            };

            eprintln!(
                "\n=== Monte Carlo Analysis ({} trades, bankroll={}U, stake={}U) ===",
                n, bankroll_start, stake
            );
            eprintln!("Actual series:");
            eprintln!(
                "  mean_pnl/trade={:.2}U  std={:.2}U  sharpe={:.2}  sortino={:.2}",
                mean_pnl, std_pnl, sharpe, sortino
            );
            eprintln!(
                "  max_drawdown={:.2}U ({:.1}% of peak equity)",
                max_dd, max_dd_pct
            );
            eprintln!(
                "  total_pnl={:.2}U  roi_on_bankroll={:.1}%",
                series.iter().sum::<f64>(),
                series.iter().sum::<f64>() / bankroll_start * 100.0
            );

            // --- Monte Carlo bootstrap ---
            const MC_RUNS: usize = 10_000;
            let mut rng_state: u64 = 0xdeadbeef_cafebabe;
            let mut mc_final_pnl = Vec::with_capacity(MC_RUNS);
            let mut mc_max_dd = Vec::with_capacity(MC_RUNS);
            let mut ruin_count = 0u32;

            for _ in 0..MC_RUNS {
                let mut equity = 0.0f64;
                let mut peak = 0.0f64;
                let mut max_dd = 0.0f64;
                let mut ruined = false;

                for _ in 0..n {
                    // xorshift64 PRNG
                    rng_state ^= rng_state << 13;
                    rng_state ^= rng_state >> 7;
                    rng_state ^= rng_state << 17;
                    let idx = (rng_state % n as u64) as usize;
                    let p = series[idx];

                    equity += p;
                    if equity > peak {
                        peak = equity;
                    }
                    let dd = peak - equity;
                    if dd > max_dd {
                        max_dd = dd;
                    }

                    // Ruin: bankroll + equity < stake (can't place next trade)
                    if bankroll_start + equity < stake {
                        ruined = true;
                    }
                }

                mc_final_pnl.push(equity);
                mc_max_dd.push(max_dd);
                if ruined {
                    ruin_count += 1;
                }
            }

            mc_final_pnl.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            mc_max_dd.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            let p5_pnl = mc_final_pnl[(MC_RUNS as f64 * 0.05) as usize];
            let p25_pnl = mc_final_pnl[(MC_RUNS as f64 * 0.25) as usize];
            let p50_pnl = mc_final_pnl[(MC_RUNS as f64 * 0.50) as usize];
            let p75_pnl = mc_final_pnl[(MC_RUNS as f64 * 0.75) as usize];
            let p95_pnl = mc_final_pnl[(MC_RUNS as f64 * 0.95) as usize];

            let p50_dd = mc_max_dd[(MC_RUNS as f64 * 0.50) as usize];
            let p90_dd = mc_max_dd[(MC_RUNS as f64 * 0.90) as usize];
            let p95_dd = mc_max_dd[(MC_RUNS as f64 * 0.95) as usize];
            let p99_dd = mc_max_dd[(MC_RUNS as f64 * 0.99) as usize];

            let ruin_prob = ruin_count as f64 / MC_RUNS as f64 * 100.0;

            eprintln!(
                "\nMonte Carlo ({} simulations, bootstrap resampling):",
                MC_RUNS
            );
            eprintln!(
                "  Final P&L distribution (same {} trades, random order):",
                n
            );
            eprintln!(
                "    p5={:.0}U  p25={:.0}U  p50={:.0}U  p75={:.0}U  p95={:.0}U",
                p5_pnl, p25_pnl, p50_pnl, p75_pnl, p95_pnl
            );
            eprintln!("  Max drawdown distribution:");
            eprintln!(
                "    p50={:.0}U  p90={:.0}U  p95={:.0}U  p99={:.0}U",
                p50_dd, p90_dd, p95_dd, p99_dd
            );
            eprintln!("  Ruin probability (bankroll < stake): {:.2}%", ruin_prob);

            // --- Strategy summary ---
            eprintln!("\n=== Strategy Core ===");
            eprintln!("Name:    ThreeLayer (State→Direction, LOB→Confirm, R/R→Filter)");
            eprintln!(
                "Markets: Polymarket 5-minute binary options on crypto (BTC/ETH/DOGE/SOL/XRP/BNB)"
            );
            eprintln!("Regimes:");
            eprintln!("  early  (271-300s): direction = contrarian(model_prob_up), gate = always");
            eprintln!(
                "  middle ( 61-270s): direction = 0.65*contrarian + 0.35*fair_prob_up_clean + vol_gap_adjust"
            );
            eprintln!("                     gate = vol_gap must agree with direction");
            eprintln!("  expiry (  0- 60s): NO TRADE (thin liquidity, PM price converged)");
            eprintln!("Layer 1 (Direction): fair_prob_up_clean, vol_gap, distance_over_sigma");
            eprintln!(
                "Layer 2 (Confirm):   drift_30s + obi_10 + depth_imbalance + cum_mprice_drift_5m"
            );
            eprintln!(
                "                     need >= {} of 4 LOB signals to agree",
                three_layer_confirmations_min
            );
            eprintln!(
                "Layer 3 (R/R):       edge >= 2%, reward_risk >= {:.1}, entry_price <= {:.2}",
                three_layer_reward_risk_min, three_layer_max_entry_price
            );
        }
    }

    // === Parameter Grid Sweep ===
    // Pre-compute per-event signals (direction + LOB votes + fill price) once,
    // then sweep filter thresholds without re-running the full simulation.
    {
        struct EventSignal {
            symbol: String,
            entry_ts: DateTime<Utc>,
            side_up: bool,
            entry_price: f64,
            fill_price: f64,
            had_slippage: bool,
            p_three_up: f64,
            confirmations: usize,
            pre_fill_rr: f64,
            pm_lag_secs: f64,
            settlement_up: f64,
            pnl_if_up: f64,
            pnl_if_down: f64,
        }

        let mut signals: Vec<EventSignal> = Vec::new();

        for observations in &all_observations {
            let mut by_event: std::collections::HashMap<
                &str,
                Vec<&ploy_research::FactorObservation>,
            > = std::collections::HashMap::new();
            for obs in observations.iter() {
                by_event.entry(obs.event_id.as_str()).or_default().push(obs);
            }
            for event_obs in by_event.values_mut() {
                event_obs.sort_by_key(|obs| obs.tick_ts);
            }

            for event_obs in by_event.values() {
                // Find first qualifying observation (same logic as one-trade sim)
                for obs in event_obs.iter() {
                    if !obs.pm_up_ask.is_finite()
                        || !obs.pm_down_ask.is_finite()
                        || !obs.model_prob_up.is_finite()
                        || !obs.fair_prob_up_clean.is_finite()
                        || !obs.reward_risk_up.is_finite()
                        || !obs.reward_risk_down.is_finite()
                    {
                        continue;
                    }

                    let regime = three_layer_regime(obs.time_remaining_secs);
                    if regime == "expiry" {
                        continue;
                    }

                    let fee_up = 0.02 * obs.pm_up_ask * (1.0 - obs.pm_up_ask);
                    let fee_down = 0.02 * obs.pm_down_ask * (1.0 - obs.pm_down_ask);
                    let independent_state_up = (1.0 - obs.model_prob_up).clamp(1e-4, 1.0 - 1e-4);
                    let p_three_up = match regime {
                        "early" => independent_state_up,
                        "middle" => {
                            let vol_adj = if obs.vol_gap.is_finite() {
                                three_layer_middle_vol_adjust * sign_vote(obs.vol_gap) as f64
                            } else {
                                0.0
                            };
                            (0.65 * independent_state_up + 0.35 * obs.fair_prob_up_clean + vol_adj)
                                .clamp(1e-4, 1.0 - 1e-4)
                        }
                        _ => 0.5,
                    };

                    let side_up_opt = if p_three_up > 0.5 {
                        Some(true)
                    } else if p_three_up < 0.5 {
                        Some(false)
                    } else {
                        None
                    };
                    let side_up_opt = side_up_opt.filter(|su| match regime {
                        "early" => true,
                        "middle" => {
                            obs.vol_gap.is_finite()
                                && ((*su && obs.vol_gap > 0.0) || (!*su && obs.vol_gap < 0.0))
                        }
                        _ => false,
                    });
                    let Some(side_up) = side_up_opt else { continue };

                    let lob_votes = [
                        sign_vote(obs.drift_30s),
                        sign_vote(obs.obi_10),
                        sign_vote(obs.depth_imbalance),
                        sign_vote(obs.cum_mprice_drift_5m),
                    ];
                    let desired = if side_up { 1 } else { -1 };
                    let confirmations = lob_votes.iter().filter(|&&v| v == desired).count();

                    let entry_price = if side_up {
                        obs.pm_up_ask
                    } else {
                        obs.pm_down_ask
                    };
                    let pre_fill_rr = if side_up {
                        obs.reward_risk_up
                    } else {
                        obs.reward_risk_down
                    };
                    let pre_fill_edge = if side_up {
                        p_three_up - (obs.pm_up_ask + fee_up)
                    } else {
                        (1.0 - p_three_up) - (obs.pm_down_ask + fee_down)
                    };
                    if pre_fill_edge < min_edge {
                        continue;
                    }

                    // Fill price (same logic as main sim)
                    let ask_size = if side_up {
                        obs.pm_up_ask_size
                    } else {
                        obs.pm_down_ask_size
                    };
                    let order_shares = stake_per_trade / entry_price;
                    let had_slippage =
                        ask_size.is_finite() && ask_size > 0.0 && ask_size < order_shares;
                    let fill_price = if had_slippage {
                        (entry_price + 0.01).min(0.99)
                    } else {
                        entry_price
                    };

                    let fill_fee = 0.02 * fill_price * (1.0 - fill_price);
                    let pnl_win = stake_per_trade * (1.0 / fill_price - 1.0)
                        - stake_per_trade * fill_fee / fill_price;
                    let pnl_lose = -stake_per_trade - stake_per_trade * fill_fee / fill_price;

                    signals.push(EventSignal {
                        symbol: obs.symbol.clone(),
                        entry_ts: obs.tick_ts,
                        side_up,
                        entry_price,
                        fill_price,
                        had_slippage,
                        p_three_up,
                        confirmations,
                        pre_fill_rr,
                        pm_lag_secs: obs.pm_lag_secs,
                        settlement_up: obs.settlement_up,
                        pnl_if_up: if obs.settlement_up == 1.0 {
                            pnl_win
                        } else {
                            pnl_lose
                        },
                        pnl_if_down: if obs.settlement_up == 0.0 {
                            pnl_win
                        } else {
                            pnl_lose
                        },
                    });
                    break; // one trade per event
                }
            }
        }

        // Grid sweep — now includes pm_lag_secs thresholds
        // max_pm_lag: skip if quote older than this (stale = unreliable)
        // min_pm_lag: only trade when quote is at least this old (stale = exploitable)
        let max_entry_prices = [0.35f64, 0.40, 0.45, 0.50, 0.55];
        let confirm_mins = [1usize, 2, 3];
        let rr_mins = [0.2f64, 0.5];
        let max_pm_lags = [5.0f64, 10.0, 15.0, f64::INFINITY]; // INFINITY = no filter
        let min_pm_lags = [0.0f64, 5.0, 10.0]; // 0 = no filter

        struct SweepResult {
            max_entry: f64,
            confirm: usize,
            rr_min: f64,
            max_pm_lag: f64,
            min_pm_lag: f64,
            trades: u32,
            win_rate: f64,
            roi: f64,
            sharpe: f64,
            max_dd: f64,
        }
        let mut sweep_results: Vec<SweepResult> = Vec::new();

        for &max_entry in &max_entry_prices {
            for &confirm in &confirm_mins {
                for &rr_min in &rr_mins {
                    for &max_pm_lag in &max_pm_lags {
                        for &min_pm_lag in &min_pm_lags {
                            let mut pnl_series: Vec<f64> = Vec::new();
                            let mut wins = 0u32;

                            for sig in &signals {
                                if sig.fill_price > max_entry {
                                    continue;
                                }
                                if sig.confirmations < confirm {
                                    continue;
                                }
                                if sig.pre_fill_rr < rr_min {
                                    continue;
                                }
                                if sig.pm_lag_secs.is_finite() {
                                    if sig.pm_lag_secs > max_pm_lag {
                                        continue;
                                    }
                                    if sig.pm_lag_secs < min_pm_lag {
                                        continue;
                                    }
                                }

                                let pnl = if sig.side_up {
                                    sig.pnl_if_up
                                } else {
                                    sig.pnl_if_down
                                };
                                let won = (sig.side_up && sig.settlement_up == 1.0)
                                    || (!sig.side_up && sig.settlement_up == 0.0);
                                pnl_series.push(pnl);
                                if won {
                                    wins += 1;
                                }
                            }

                            let n = pnl_series.len();
                            if n < 5 {
                                continue;
                            }

                            let total_pnl: f64 = pnl_series.iter().sum();
                            let total_stake = n as f64 * stake_per_trade;
                            let roi = total_pnl / total_stake * 100.0;
                            let win_rate = wins as f64 / n as f64 * 100.0;

                            let mean = total_pnl / n as f64;
                            let std = (pnl_series.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
                                / n as f64)
                                .sqrt();
                            let sharpe = if std > 1e-9 {
                                mean / std * (6.0_f64 * 365.0).sqrt()
                            } else {
                                0.0
                            };

                            let mut peak = 0.0f64;
                            let mut equity = 0.0f64;
                            let mut max_dd = 0.0f64;
                            for &p in &pnl_series {
                                equity += p;
                                if equity > peak {
                                    peak = equity;
                                }
                                let dd = peak - equity;
                                if dd > max_dd {
                                    max_dd = dd;
                                }
                            }

                            sweep_results.push(SweepResult {
                                max_entry,
                                confirm,
                                rr_min,
                                max_pm_lag,
                                min_pm_lag,
                                trades: n as u32,
                                win_rate,
                                roi,
                                sharpe,
                                max_dd,
                            });
                        } // min_pm_lag
                    } // max_pm_lag
                }
            }
        }

        // Sort by Sharpe descending
        sweep_results.sort_by(|a, b| {
            b.sharpe
                .partial_cmp(&a.sharpe)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        eprintln!(
            "\n=== Parameter Grid Sweep (top 20 by Sharpe, {} pre-qualified events) ===",
            signals.len()
        );
        eprintln!(
            "{:<8} {:<8} {:<6} {:<10} {:<10} {:<7} {:<7} {:<8} {:<8} {:<8}",
            "max_ent",
            "confirm",
            "rr",
            "max_lag",
            "min_lag",
            "trades",
            "wr%",
            "roi%",
            "sharpe",
            "max_dd"
        );
        for r in sweep_results.iter().take(20) {
            let max_lag_str = if r.max_pm_lag.is_infinite() {
                "∞".to_string()
            } else {
                format!("{:.0}", r.max_pm_lag)
            };
            eprintln!(
                "{:<8.2} {:<8} {:<6.1} {:<10} {:<10.0} {:<7} {:<7.1} {:<8.1} {:<8.2} {:<8.0}",
                r.max_entry,
                r.confirm,
                r.rr_min,
                max_lag_str,
                r.min_pm_lag,
                r.trades,
                r.win_rate,
                r.roi,
                r.sharpe,
                r.max_dd
            );
        }

        // Per-symbol breakdown for the current (default) parameters
        eprintln!(
            "\n=== Per-Symbol Breakdown (max_entry={:.2}, confirm>={}, rr>={:.1}) ===",
            three_layer_max_entry_price, three_layer_confirmations_min, three_layer_reward_risk_min
        );
        eprintln!(
            "{:<12} {:<7} {:<7} {:<8} {:<8} {:<10}",
            "symbol", "trades", "wr%", "roi%", "avg_entry", "slippage%"
        );
        let mut sym_map: std::collections::HashMap<&str, (u32, u32, f64, f64, f64, u32)> =
            std::collections::HashMap::new();
        for sig in &signals {
            if sig.fill_price > three_layer_max_entry_price {
                continue;
            }
            if sig.confirmations < three_layer_confirmations_min {
                continue;
            }
            if sig.pre_fill_rr < three_layer_reward_risk_min {
                continue;
            }
            let pnl = if sig.side_up {
                sig.pnl_if_up
            } else {
                sig.pnl_if_down
            };
            let won = (sig.side_up && sig.settlement_up == 1.0)
                || (!sig.side_up && sig.settlement_up == 0.0);
            let e = sym_map
                .entry(sig.symbol.as_str())
                .or_insert((0, 0, 0.0, 0.0, 0.0, 0));
            e.0 += 1;
            if won {
                e.1 += 1;
            }
            e.2 += pnl;
            e.3 += stake_per_trade;
            e.4 += sig.fill_price;
            if sig.had_slippage {
                e.5 += 1;
            }
        }
        let mut sym_vec: Vec<_> = sym_map.iter().collect();
        sym_vec.sort_by(|a, b| {
            b.1.2
                .partial_cmp(&a.1.2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for (sym, (trades, wins, pnl, stake, price_sum, slippage_count)) in &sym_vec {
            let wr = *wins as f64 / *trades as f64 * 100.0;
            let roi = pnl / stake * 100.0;
            let avg_entry = price_sum / *trades as f64;
            let slippage_pct = *slippage_count as f64 / *trades as f64 * 100.0;
            eprintln!(
                "{:<12} {:<7} {:<7.1} {:<8.1} {:<8.3} {:<10.1}",
                sym, trades, wr, roi, avg_entry, slippage_pct
            );
        }

        // === Per-Symbol Grid Sweep ===
        // Find optimal parameters for each symbol independently.
        let all_symbols: Vec<String> = {
            let mut seen = std::collections::HashSet::new();
            signals
                .iter()
                .filter(|s| seen.insert(s.symbol.clone()))
                .map(|s| s.symbol.clone())
                .collect()
        };

        eprintln!("\n=== Per-Symbol Best Parameters (by Sharpe) ===");
        eprintln!(
            "{:<12} {:<8} {:<8} {:<6} {:<7} {:<7} {:<8} {:<8} {:<8}",
            "symbol", "max_ent", "confirm", "rr", "trades", "wr%", "roi%", "sharpe", "max_dd"
        );

        // Store best params per symbol for portfolio construction
        let mut best_params: std::collections::HashMap<String, (f64, usize, f64)> =
            std::collections::HashMap::new();

        for sym in &all_symbols {
            let mut best: Option<(f64, usize, f64, u32, f64, f64, f64, f64)> = None; // (max_entry, confirm, rr, trades, wr, roi, sharpe, max_dd)

            for &max_entry in &max_entry_prices {
                for &confirm in &confirm_mins {
                    for &rr_min in &rr_mins {
                        let mut pnl_series: Vec<f64> = Vec::new();
                        let mut wins = 0u32;

                        for sig in signals.iter().filter(|s| &s.symbol == sym) {
                            if sig.fill_price > max_entry {
                                continue;
                            }
                            if sig.confirmations < confirm {
                                continue;
                            }
                            if sig.pre_fill_rr < rr_min {
                                continue;
                            }
                            let pnl = if sig.side_up {
                                sig.pnl_if_up
                            } else {
                                sig.pnl_if_down
                            };
                            let won = (sig.side_up && sig.settlement_up == 1.0)
                                || (!sig.side_up && sig.settlement_up == 0.0);
                            pnl_series.push(pnl);
                            if won {
                                wins += 1;
                            }
                        }

                        let n = pnl_series.len();
                        if n < 3 {
                            continue;
                        }

                        let total_pnl: f64 = pnl_series.iter().sum();
                        let total_stake = n as f64 * stake_per_trade;
                        let roi = total_pnl / total_stake * 100.0;
                        let wr = wins as f64 / n as f64 * 100.0;
                        let mean = total_pnl / n as f64;
                        let std = (pnl_series.iter().map(|x| (x - mean).powi(2)).sum::<f64>()
                            / n as f64)
                            .sqrt();
                        let sharpe = if std > 1e-9 {
                            mean / std * (6.0_f64 * 365.0).sqrt()
                        } else {
                            0.0
                        };

                        let mut peak = 0.0f64;
                        let mut equity = 0.0f64;
                        let mut max_dd = 0.0f64;
                        for &p in &pnl_series {
                            equity += p;
                            if equity > peak {
                                peak = equity;
                            }
                            let dd = peak - equity;
                            if dd > max_dd {
                                max_dd = dd;
                            }
                        }

                        if best.is_none() || sharpe > best.unwrap().6 {
                            best = Some((
                                max_entry, confirm, rr_min, n as u32, wr, roi, sharpe, max_dd,
                            ));
                        }
                    }
                }
            }

            if let Some((me, co, rr, tr, wr, roi, sh, dd)) = best {
                eprintln!(
                    "{:<12} {:<8.2} {:<8} {:<6.1} {:<7} {:<7.1} {:<8.1} {:<8.2} {:<8.0}",
                    sym, me, co, rr, tr, wr, roi, sh, dd
                );
                best_params.insert(sym.clone(), (me, co, rr));
            }
        }

        // === Portfolio Equity Curve ===
        // Use each symbol's best parameters, combine trades time-ordered.
        // Sort signals by entry_ts, apply per-symbol best params.
        let mut portfolio_trades: Vec<(DateTime<Utc>, &str, f64)> = Vec::new(); // (ts, symbol, pnl)

        for sig in &signals {
            let (max_entry, confirm, rr_min) = best_params.get(&sig.symbol).copied().unwrap_or((
                three_layer_max_entry_price,
                three_layer_confirmations_min,
                three_layer_reward_risk_min,
            ));

            if sig.fill_price > max_entry {
                continue;
            }
            if sig.confirmations < confirm {
                continue;
            }
            if sig.pre_fill_rr < rr_min {
                continue;
            }

            let pnl = if sig.side_up {
                sig.pnl_if_up
            } else {
                sig.pnl_if_down
            };
            portfolio_trades.push((sig.entry_ts, sig.symbol.as_str(), pnl));
        }

        portfolio_trades.sort_by_key(|(ts, _, _)| *ts);

        // Print equity curve (one row per trade, cumulative P&L)
        eprintln!(
            "\n=== Portfolio Equity Curve (per-symbol optimal params, {} trades) ===",
            portfolio_trades.len()
        );
        eprintln!(
            "{:<26} {:<12} {:<8} {:<10} {:<10}",
            "ts", "symbol", "pnl", "cum_pnl", "drawdown"
        );
        let mut cum_pnl = 0.0f64;
        let mut peak_pnl = 0.0f64;
        let mut max_portfolio_dd = 0.0f64;
        let mut portfolio_wins = 0u32;
        for (ts, sym, pnl) in &portfolio_trades {
            cum_pnl += pnl;
            if cum_pnl > peak_pnl {
                peak_pnl = cum_pnl;
            }
            let dd = peak_pnl - cum_pnl;
            if dd > max_portfolio_dd {
                max_portfolio_dd = dd;
            }
            if *pnl > 0.0 {
                portfolio_wins += 1;
            }
            eprintln!(
                "{:<26} {:<12} {:<8.2} {:<10.2} {:<10.2}",
                ts.format("%Y-%m-%d %H:%M:%S"),
                sym,
                pnl,
                cum_pnl,
                dd
            );
        }
        let n_port = portfolio_trades.len();
        let port_wr = if n_port > 0 {
            portfolio_wins as f64 / n_port as f64 * 100.0
        } else {
            0.0
        };
        let port_roi = if n_port > 0 {
            cum_pnl / (n_port as f64 * stake_per_trade) * 100.0
        } else {
            0.0
        };
        eprintln!(
            "\nPortfolio summary: trades={} wins={} wr={:.1}% total_pnl={:.2}U roi={:.1}% max_dd={:.0}U",
            n_port, portfolio_wins, port_wr, cum_pnl, port_roi, max_portfolio_dd
        );

        // === Walk-Forward Validation ===
        // Split signals chronologically: first half = train, second half = test.
        // Use median timestamp as split point (robust even if all signals are same date).
        if signals.len() >= 20 {
            let mut sorted_signals: Vec<&EventSignal> = signals.iter().collect();
            sorted_signals.sort_by_key(|s| s.entry_ts);
            let mid_idx = sorted_signals.len() / 2;
            let split_ts = sorted_signals[mid_idx].entry_ts;

            let train_signals: Vec<&EventSignal> =
                signals.iter().filter(|s| s.entry_ts < split_ts).collect();
            let test_signals: Vec<&EventSignal> =
                signals.iter().filter(|s| s.entry_ts >= split_ts).collect();

            eprintln!("\n=== Walk-Forward Validation ===");
            eprintln!(
                "Train: before {} ({} signals)",
                split_ts.format("%Y-%m-%d %H:%M"),
                train_signals.len()
            );
            eprintln!(
                "Test:  from   {} ({} signals)",
                split_ts.format("%Y-%m-%d %H:%M"),
                test_signals.len()
            );

            // Find best params on train set
            let mut best_train: Option<(f64, usize, f64, f64)> = None; // (max_entry, confirm, rr, sharpe)
            for &max_entry in &max_entry_prices {
                for &confirm in &confirm_mins {
                    for &rr_min in &rr_mins {
                        let pnl_series: Vec<f64> = train_signals
                            .iter()
                            .filter(|s| {
                                s.fill_price <= max_entry
                                    && s.confirmations >= confirm
                                    && s.pre_fill_rr >= rr_min
                            })
                            .map(|s| {
                                if s.side_up {
                                    s.pnl_if_up
                                } else {
                                    s.pnl_if_down
                                }
                            })
                            .collect();
                        if pnl_series.len() < 10 {
                            continue;
                        }
                        let n = pnl_series.len() as f64;
                        let mean = pnl_series.iter().sum::<f64>() / n;
                        let std =
                            (pnl_series.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n).sqrt();
                        let sharpe = if std > 0.0 {
                            mean / std * (6.0 * 365.0_f64).sqrt()
                        } else {
                            0.0
                        };
                        if best_train.map_or(true, |(_, _, _, s)| sharpe > s) {
                            best_train = Some((max_entry, confirm, rr_min, sharpe));
                        }
                    }
                }
            }

            if let Some((best_max_entry, best_confirm, best_rr, train_sharpe)) = best_train {
                // Evaluate on test set with train-optimized params
                let test_pnl: Vec<f64> = test_signals
                    .iter()
                    .filter(|s| {
                        s.fill_price <= best_max_entry
                            && s.confirmations >= best_confirm
                            && s.pre_fill_rr >= best_rr
                    })
                    .map(|s| {
                        if s.side_up {
                            s.pnl_if_up
                        } else {
                            s.pnl_if_down
                        }
                    })
                    .collect();

                let test_n = test_pnl.len();
                let test_wins = test_pnl.iter().filter(|&&p| p > 0.0).count();
                let test_total = test_pnl.iter().sum::<f64>();
                let test_stake = test_n as f64 * stake_per_trade;
                let test_wr = if test_n > 0 {
                    test_wins as f64 / test_n as f64 * 100.0
                } else {
                    0.0
                };
                let test_roi = if test_stake > 0.0 {
                    test_total / test_stake * 100.0
                } else {
                    0.0
                };
                let test_mean = if test_n > 0 {
                    test_total / test_n as f64
                } else {
                    0.0
                };
                let test_std = if test_n > 1 {
                    (test_pnl
                        .iter()
                        .map(|x| (x - test_mean).powi(2))
                        .sum::<f64>()
                        / test_n as f64)
                        .sqrt()
                } else {
                    0.0
                };
                let test_sharpe = if test_std > 0.0 {
                    test_mean / test_std * (6.0 * 365.0_f64).sqrt()
                } else {
                    0.0
                };

                // Also evaluate train set with same params for comparison
                let train_pnl: Vec<f64> = train_signals
                    .iter()
                    .filter(|s| {
                        s.fill_price <= best_max_entry
                            && s.confirmations >= best_confirm
                            && s.pre_fill_rr >= best_rr
                    })
                    .map(|s| {
                        if s.side_up {
                            s.pnl_if_up
                        } else {
                            s.pnl_if_down
                        }
                    })
                    .collect();
                let train_n = train_pnl.len();
                let train_wins = train_pnl.iter().filter(|&&p| p > 0.0).count();
                let train_total = train_pnl.iter().sum::<f64>();
                let train_stake = train_n as f64 * stake_per_trade;
                let train_wr = if train_n > 0 {
                    train_wins as f64 / train_n as f64 * 100.0
                } else {
                    0.0
                };
                let train_roi = if train_stake > 0.0 {
                    train_total / train_stake * 100.0
                } else {
                    0.0
                };

                eprintln!(
                    "Best params (by train Sharpe): max_entry={:.2} confirm>={} rr>={:.1}",
                    best_max_entry, best_confirm, best_rr
                );
                eprintln!(
                    "{:<8} {:<8} {:<8} {:<8} {:<8} {:<8}",
                    "period", "trades", "wr%", "roi%", "sharpe", "verdict"
                );
                eprintln!(
                    "{:<8} {:<8} {:<8.1} {:<8.1} {:<8.2} {:<8}",
                    "train", train_n, train_wr, train_roi, train_sharpe, "in-sample"
                );
                eprintln!(
                    "{:<8} {:<8} {:<8.1} {:<8.1} {:<8.2} {:<8}",
                    "test",
                    test_n,
                    test_wr,
                    test_roi,
                    test_sharpe,
                    if test_sharpe > 1.0 {
                        "VALID"
                    } else if test_sharpe > 0.0 {
                        "WEAK"
                    } else {
                        "FAIL"
                    }
                );

                let decay = if train_sharpe > 0.0 {
                    (1.0 - test_sharpe / train_sharpe) * 100.0
                } else {
                    0.0
                };
                eprintln!(
                    "Sharpe decay: {:.1}% (train={:.2} → test={:.2})",
                    decay, train_sharpe, test_sharpe
                );
                if test_wr > 55.0 && test_roi > 0.0 {
                    eprintln!("Verdict: OUT-OF-SAMPLE POSITIVE — signal likely real");
                } else if test_wr > 50.0 {
                    eprintln!("Verdict: MARGINAL — weak signal, needs more data");
                } else {
                    eprintln!("Verdict: OVERFIT — in-sample only, no real edge");
                }
            }
        }
    }

    // Print final multi-factor weights
    if let Some((weights, _, _)) = mf_model.fit() {
        let names = [
            "obi_10",
            "depth_imbal",
            "depth_accel",
            "spread_bps",
            "cum_obi_d5m",
            "cum_dep_d5m",
            "cum_mp_d5m",
            "cum_trd_i5m",
            "drift_10s",
            "drift_30s",
            "pm_lag_secs",
        ];
        eprintln!(
            "\n=== Multi-Factor Weights (IC vs settlement, {} obs) ===",
            mf_model.data.len()
        );
        for (i, name) in names.iter().enumerate() {
            eprintln!("  {:<14} w={:>7.4}", name, weights[i]);
        }
    }
    eprintln!("mf_model obs={}", mf_model.data.len());

    // === Calibration Statistics (full hindsight, for diagnostic only) ===
    {
        const N_BUCKETS: usize = 20;
        let mut pairs: Vec<(f64, f64)> = cal_data.clone();
        pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
        if !pairs.is_empty() {
            let bucket_size = (pairs.len() / N_BUCKETS).max(1);
            eprintln!(
                "\n=== Calibration: d/σ → P(up) ({} obs, {} per bucket) ===",
                pairs.len(),
                bucket_size
            );
            eprintln!(
                "{:<12} {:<12} {:<8} {:<12} {:<12}",
                "d/σ_lo", "d/σ_hi", "n", "emp_win%", "model_cdf%"
            );
            for chunk in pairs.chunks(bucket_size) {
                if chunk.is_empty() {
                    continue;
                }
                let lo = chunk.first().unwrap().0;
                let hi = chunk.last().unwrap().0;
                let n = chunk.len();
                let wins: f64 = chunk.iter().map(|(_, s)| s).sum();
                let emp_rate = wins / n as f64 * 100.0;
                let mid_z = (lo + hi) / 2.0;
                let model_cdf = {
                    let sign = if mid_z < 0.0 { -1.0 } else { 1.0 };
                    let x = mid_z.abs();
                    let t = 1.0 / (1.0 + 0.3275911 * x);
                    let poly = t
                        * (0.254829592
                            + t * (-0.284496736
                                + t * (1.421413741 + t * (-1.453152027 + t * 1.061405429))));
                    0.5 * (1.0 + sign * (1.0 - poly * (-x * x / 2.0).exp()))
                };
                eprintln!(
                    "{:<12.4} {:<12.4} {:<8} {:<12.1} {:<12.1}",
                    lo,
                    hi,
                    n,
                    emp_rate,
                    model_cdf * 100.0
                );
            }
        }
    }

    let mut settlement_metrics: Vec<_> = aggregated
        .iter()
        .filter(|metric| metric.label == "settlement_up" && metric.mean_spearman_ic.is_finite())
        .collect();
    settlement_metrics
        .sort_by(|a, b| descending_abs_f64_cmp(a.mean_spearman_ic, b.mean_spearman_ic));

    let mut lag_metrics: Vec<_> = aggregated
        .iter()
        .filter(|metric| {
            metric.label == "future_up_ask_change_30s" && metric.mean_spearman_ic.is_finite()
        })
        .collect();
    lag_metrics.sort_by(|a, b| descending_abs_f64_cmp(a.mean_spearman_ic, b.mean_spearman_ic));

    eprintln!("\n=== Settlement Factors (Top 10 by |Mean Spearman IC|) ===");
    for metric in settlement_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} windows={:<4} mean_n={:<8.1} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.windows,
            metric.mean_n,
            metric.mean_pearson_ic,
            metric.mean_spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    eprintln!("\n=== PM Lag Factors (Top 10 by |Mean Spearman IC|) ===");
    for metric in lag_metrics.into_iter().take(10) {
        eprintln!(
            "{:<24} windows={:<4} mean_n={:<8.1} pearson={:>7.4} spearman={:>7.4} icir={}",
            metric.factor,
            metric.windows,
            metric.mean_n,
            metric.mean_pearson_ic,
            metric.mean_spearman_ic,
            metric
                .icir
                .map(|value| format!("{value:>7.4}"))
                .unwrap_or_else(|| "   n/a".to_string())
        );
    }

    eprintln!("\n=== Window Count ===");
    eprintln!("{}", windows.len());
}
