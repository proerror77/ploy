//! Build the next Research Manager plan from durable Research OS trace tables.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use ploy_research::research_os::manager::{
    ResearchBudget, ResearchManagerInput, plan_next_research,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|window| window[0] == flag)
        .map(|window| window[1].clone())
}

fn parse_usize(args: &[String], flag: &str, default: usize) -> usize {
    flag_value(args, flag)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(default)
}

fn usage() -> &'static str {
    "usage: research_trace_plan --db-url <url>|DATABASE_URL [--evidence-stage factor_attribution] [--limit 20] [--output <path>]"
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let db_url = flag_value(&args, "--db-url")
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .with_context(|| usage())?;
    let evidence_stage =
        flag_value(&args, "--evidence-stage").unwrap_or_else(|| "factor_attribution".to_string());
    let limit = parse_usize(&args, "--limit", 20).min(100);
    let output = flag_value(&args, "--output").map(PathBuf::from);

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(120))
        .connect(&db_url)
        .await?;

    let input = build_manager_input(&pool, &evidence_stage, limit).await?;
    let plan = plan_next_research(&input).map_err(anyhow::Error::msg)?;
    let payload = json!({
        "schema_version": "research_trace_plan.v1",
        "input": input,
        "plan": plan,
    });
    let rendered = serde_json::to_string_pretty(&payload)?;

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, rendered + "\n")
            .with_context(|| format!("write output {}", path.display()))?;
    } else {
        println!("{rendered}");
    }

    Ok(())
}

async fn build_manager_input(
    pool: &PgPool,
    evidence_stage: &str,
    limit: usize,
) -> Result<ResearchManagerInput> {
    Ok(ResearchManagerInput {
        evidence_stage: evidence_stage.to_string(),
        latest_runs: latest_runs(pool, evidence_stage, limit).await?,
        factor_registry_summary: factor_registry_summary(pool, limit).await?,
        rejected_factor_patterns: rejected_factor_patterns(pool, limit).await?,
        market_data_health: latest_snapshot_health(pool).await?,
        research_budget: ResearchBudget {
            max_candidates_per_day: 20,
            max_backtests_per_day: 4,
            max_llm_calls_per_day: 2,
        },
    })
}

async fn latest_runs(pool: &PgPool, evidence_stage: &str, limit: usize) -> Result<Value> {
    let rows: Vec<(String, String, Option<String>, Value, DateTime<Utc>)> = sqlx::query_as(
        r#"
        SELECT run_id, event_type, promotion_decision, output_json, created_at
        FROM experiment_trace
        WHERE evidence_stage = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
    )
    .bind(evidence_stage)
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "source": "experiment_trace",
        "evidence_stage": evidence_stage,
        "runs": rows.into_iter().map(|row| {
            json!({
                "run_id": row.0,
                "event_type": row.1,
                "promotion_decision": row.2,
                "output_json": row.3,
                "created_at": row.4,
            })
        }).collect::<Vec<_>>()
    }))
}

async fn factor_registry_summary(pool: &PgPool, limit: usize) -> Result<Value> {
    let status_counts: Vec<(String, i64)> = sqlx::query_as(
        r#"
        SELECT status, COUNT(*)::BIGINT
        FROM factor_registry
        GROUP BY status
        ORDER BY status
        "#,
    )
    .fetch_all(pool)
    .await?;
    let recent: Vec<(String, String, String, String, String, Value)> = sqlx::query_as(
        r#"
        SELECT factor_name, dsl_hash, target, horizon, status, runtime_contract
        FROM factor_registry
        ORDER BY created_at DESC
        LIMIT $1
        "#,
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "source": "factor_registry",
        "status_counts": status_counts.into_iter().map(|row| {
            json!({"status": row.0, "count": row.1})
        }).collect::<Vec<_>>(),
        "recent_factors": recent.into_iter().map(|row| {
            json!({
                "factor_name": row.0,
                "dsl_hash": row.1,
                "target": row.2,
                "horizon": row.3,
                "status": row.4,
                "runtime_contract": row.5,
            })
        }).collect::<Vec<_>>()
    }))
}

async fn rejected_factor_patterns(pool: &PgPool, limit: usize) -> Result<Value> {
    let rows: Vec<(String, String, Value, i64)> = sqlx::query_as(
        r#"
        SELECT promotion_decision, promotion_status, blockers_json, COUNT(*)::BIGINT
        FROM factor_evaluations
        WHERE promotion_status IN ('blocked', 'rejected')
           OR promotion_decision IN ('blocked', 'reject', 'revise')
        GROUP BY promotion_decision, promotion_status, blockers_json
        ORDER BY COUNT(*) DESC
        LIMIT $1
        "#,
    )
    .bind(limit as i64)
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "source": "factor_evaluations",
        "patterns": rows.into_iter().map(|row| {
            json!({
                "promotion_decision": row.0,
                "promotion_status": row.1,
                "blockers_json": row.2,
                "count": row.3,
            })
        }).collect::<Vec<_>>()
    }))
}

async fn latest_snapshot_health(pool: &PgPool) -> Result<Value> {
    let row: Option<(String, DateTime<Utc>, DateTime<Utc>, Value, Value)> = sqlx::query_as(
        r#"
        SELECT data_snapshot_id, dataset_start_ts, dataset_end_ts, source_surfaces_json, row_counts_json
        FROM research_dataset_snapshots
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    Ok(match row {
        Some(row) => json!({
            "source": "research_dataset_snapshots",
            "data_snapshot_id": row.0,
            "dataset_start_ts": row.1,
            "dataset_end_ts": row.2,
            "source_surfaces": row.3,
            "row_counts": row.4,
        }),
        None => json!({
            "source": "research_dataset_snapshots",
            "missing_blocks_promotion": true,
            "reason": "no_research_dataset_snapshots",
        }),
    })
}
