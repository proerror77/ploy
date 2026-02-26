//! Data integrity checker
//!
//! Runs comprehensive checks against the database to detect:
//! - Duplicate ticks and event versions
//! - Orphaned fills
//! - DLQ over-retry entries
//! - Stale open positions
//! - Unresolved discrepancies
//! - Position avg_entry_price drift from fills
//!
//! Usage:
//!   ploy strategy integrity-check [--json]

use anyhow::Result;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{info, warn};

/// Result of a single integrity check
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub ok: bool,
    pub count: i64,
    pub detail: Option<String>,
}

/// Full integrity report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrityReport {
    pub healthy: bool,
    pub checks: Vec<CheckResult>,
}

impl std::fmt::Display for IntegrityReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let status = if self.healthy { "HEALTHY" } else { "UNHEALTHY" };
        writeln!(f, "=== Data Integrity Report: {} ===", status)?;
        for check in &self.checks {
            let icon = if check.ok { "OK" } else { "FAIL" };
            write!(f, "  [{:>4}] {} (count: {})", icon, check.name, check.count)?;
            if let Some(ref detail) = check.detail {
                write!(f, " — {}", detail)?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

pub struct IntegrityChecker {
    pool: PgPool,
}

impl IntegrityChecker {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Run the full integrity check suite.
    ///
    /// 1. Calls the SQL `check_data_integrity()` function (migration 019)
    /// 2. Cross-checks position avg_entry_price against fills
    pub async fn run_full_check(&self) -> Result<IntegrityReport> {
        let mut checks = Vec::new();

        // ── Phase 1: SQL-level checks ────────────────────────────
        match self.run_sql_checks().await {
            Ok(sql_checks) => checks.extend(sql_checks),
            Err(e) => {
                warn!("SQL integrity function not available (run migration 019?): {}", e);
                checks.push(CheckResult {
                    name: "sql_check_data_integrity".into(),
                    ok: false,
                    count: -1,
                    detail: Some(format!("Function unavailable: {}", e)),
                });
            }
        }

        // ── Phase 2: Rust-level cross-checks ─────────────────────
        match self.check_avg_entry_price_drift().await {
            Ok(drift_check) => checks.push(drift_check),
            Err(e) => {
                warn!("avg_entry_price drift check failed: {}", e);
                checks.push(CheckResult {
                    name: "avg_entry_price_drift".into(),
                    ok: false,
                    count: -1,
                    detail: Some(format!("Check error: {}", e)),
                });
            }
        }

        let healthy = checks.iter().all(|c| c.ok);
        let report = IntegrityReport { healthy, checks };

        if healthy {
            info!("Integrity check passed: all checks OK");
        } else {
            warn!("Integrity check found issues — see report for details");
        }

        Ok(report)
    }

    /// Call the PL/pgSQL `check_data_integrity()` function and parse results.
    async fn run_sql_checks(&self) -> Result<Vec<CheckResult>> {
        let row: (serde_json::Value,) =
            sqlx::query_as("SELECT check_data_integrity()")
                .fetch_one(&self.pool)
                .await?;

        let json = row.0;
        let mut checks = Vec::new();

        // Parse each key from the JSONB result
        let check_keys = [
            "duplicate_ticks",
            "orphaned_fills",
            "dlq_over_retry",
            "duplicate_event_versions",
            "stale_open_positions",
            "unresolved_discrepancies_24h",
        ];

        for key in &check_keys {
            if let Some(entry) = json.get(key) {
                let count = entry.get("count").and_then(|v| v.as_i64()).unwrap_or(-1);
                let ok = entry.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
                checks.push(CheckResult {
                    name: key.to_string(),
                    ok,
                    count,
                    detail: None,
                });
            }
        }

        Ok(checks)
    }

    /// Cross-check: recompute avg_entry_price from fills and compare to positions table.
    ///
    /// For each open position, sum(fills.price * fills.shares) / sum(fills.shares)
    /// should match positions.avg_entry_price within a small tolerance.
    async fn check_avg_entry_price_drift(&self) -> Result<CheckResult> {
        let rows: Vec<(i32, Decimal, Decimal)> = sqlx::query_as(
            r#"
            SELECT
                p.id,
                p.avg_entry_price,
                COALESCE(
                    SUM(f.price * f.shares) / NULLIF(SUM(f.shares), 0),
                    p.avg_entry_price
                ) AS computed_avg
            FROM positions p
            LEFT JOIN fills f ON f.position_id = p.id
            WHERE p.status = 'OPEN'
            GROUP BY p.id, p.avg_entry_price
            HAVING ABS(
                p.avg_entry_price - COALESCE(
                    SUM(f.price * f.shares) / NULLIF(SUM(f.shares), 0),
                    p.avg_entry_price
                )
            ) > 0.001
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        let count = rows.len() as i64;
        let detail = if count > 0 {
            let ids: Vec<String> = rows.iter().take(5).map(|(id, _, _)| id.to_string()).collect();
            Some(format!("Drifted position IDs (first 5): {}", ids.join(", ")))
        } else {
            None
        };

        Ok(CheckResult {
            name: "avg_entry_price_drift".into(),
            ok: count == 0,
            count,
            detail,
        })
    }
}
