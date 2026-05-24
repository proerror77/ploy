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
    let rows: Vec<(String, DateTime<Utc>, Value)> = sqlx::query_as(
        r#"
        SELECT
            run_id,
            MAX(created_at) AS latest_created_at,
            jsonb_agg(
                jsonb_build_object(
                    'event_type', event_type,
                    'promotion_decision', promotion_decision,
                    'output_json', output_json,
                    'created_at', created_at
                )
                ORDER BY created_at DESC
            ) AS artifacts
        FROM experiment_trace
        WHERE evidence_stage = $1
        GROUP BY run_id
        ORDER BY latest_created_at DESC
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
                "latest_created_at": row.1,
                "artifacts": row.2,
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
    let recent: Vec<(String, String, String, String, String, Value, Value)> = sqlx::query_as(
        r#"
        SELECT factor_name, dsl_hash, target, horizon, status, runtime_contract, blockers_json
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
                "blockers": row.6,
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
        Some(row) => {
            let execution_surfaces =
                latest_full_depth_execution_surfaces(pool, row.1, row.2).await?;
            let surface_blockers = source_surface_blockers(&row.3, &execution_surfaces);
            let data_repair_blockers = blockers_by_type(&surface_blockers, "data_repair");
            let promotion_blockers = blockers_by_type(&surface_blockers, "promotion");
            let has_surface_blockers = !surface_blockers.is_empty();
            let has_data_repair_blockers = !data_repair_blockers.is_empty();
            json!({
                "source": "research_dataset_snapshots",
                "data_snapshot_id": row.0,
                "dataset_start_ts": row.1,
                "dataset_end_ts": row.2,
                "source_surfaces": row.3,
                "row_counts": row.4,
                "execution_surfaces": execution_surfaces,
                "surface_blockers": surface_blockers,
                "data_repair_blockers": data_repair_blockers,
                "promotion_blockers": promotion_blockers,
                "missing_blocks_promotion": has_surface_blockers,
                "critical_missing": has_data_repair_blockers,
            })
        }
        None => json!({
            "source": "research_dataset_snapshots",
            "missing_blocks_promotion": true,
            "reason": "no_research_dataset_snapshots",
        }),
    })
}

async fn latest_full_depth_execution_surfaces(
    pool: &PgPool,
    dataset_start_ts: DateTime<Utc>,
    dataset_end_ts: DateTime<Utc>,
) -> Result<Value> {
    let rows: Vec<(
        String,
        String,
        String,
        DateTime<Utc>,
        DateTime<Utc>,
        i32,
        i32,
        i64,
        bool,
        bool,
        Value,
    )> = sqlx::query_as(
        r#"
            SELECT
                full_depth_execution_surface_id,
                surface,
                source,
                window_start_ts,
                window_end_ts,
                checked_hours,
                existing_hours,
                row_count,
                full_fidelity,
                incomplete,
                blockers_json
            FROM full_depth_execution_surfaces
            WHERE valid = true
              AND window_start_ts <= $1
              AND window_end_ts >= $2
            ORDER BY created_at DESC
            LIMIT 50
            "#,
    )
    .bind(dataset_start_ts)
    .bind(dataset_end_ts)
    .fetch_all(pool)
    .await?;

    Ok(json!({
        "source": "full_depth_execution_surfaces",
        "dataset_start_ts": dataset_start_ts,
        "dataset_end_ts": dataset_end_ts,
        "surfaces": rows.into_iter().map(|row| {
            json!({
                "full_depth_execution_surface_id": row.0,
                "surface": row.1,
                "source": row.2,
                "window_start_ts": row.3,
                "window_end_ts": row.4,
                "checked_hours": row.5,
                "existing_hours": row.6,
                "row_count": row.7,
                "full_fidelity": row.8,
                "incomplete": row.9,
                "blockers": row.10,
            })
        }).collect::<Vec<_>>()
    }))
}

fn blockers_by_type(blockers: &[Value], blocker_type: &str) -> Vec<Value> {
    blockers
        .iter()
        .filter(|blocker| {
            blocker
                .get("blocker_type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == blocker_type)
        })
        .cloned()
        .collect()
}

fn source_surface_blockers(source_surfaces: &Value, execution_surfaces: &Value) -> Vec<Value> {
    let Some(items) = source_surfaces.as_array() else {
        return vec![json!({
            "surface": "<invalid>",
            "reason": "source_surfaces_json_not_array",
        })];
    };

    items
        .iter()
        .filter_map(|surface| {
            let name = surface
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("<unknown>");
            let gate_category = surface
                .get("gate_category")
                .and_then(Value::as_str)
                .unwrap_or("optional_context");
            let row_count = surface.get("row_count").and_then(Value::as_u64);
            let snapshot_sampled = surface
                .get("snapshot_sampled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let requires_execution = gate_category == "required_for_execution";
            let full_depth_covered = execution_surface_covered(execution_surfaces, name);

            let blocker = if gate_category == "missing_blocks_promotion" {
                Some(("surface_declared_missing_blocks_promotion", "promotion"))
            } else if requires_execution && row_count == Some(0) {
                Some(("required_execution_surface_empty", "data_repair"))
            } else if requires_execution && snapshot_sampled && full_depth_covered {
                None
            } else if requires_execution && snapshot_sampled {
                Some((
                    "required_execution_surface_is_sampled_snapshot",
                    "promotion",
                ))
            } else if requires_execution && row_count.is_none() {
                Some(("required_execution_surface_not_materialized", "promotion"))
            } else {
                None
            };

            blocker.map(|(reason, blocker_type)| {
                json!({
                    "surface": name,
                    "gate_category": gate_category,
                    "reason": reason,
                    "blocker_type": blocker_type,
                })
            })
        })
        .collect()
}

fn execution_surface_covered(execution_surfaces: &Value, surface_name: &str) -> bool {
    execution_surfaces
        .get("surfaces")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("surface").and_then(Value::as_str) == Some(surface_name)
                    && item
                        .get("full_fidelity")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    && !item
                        .get("incomplete")
                        .and_then(Value::as_bool)
                        .unwrap_or(true)
            })
        })
}
