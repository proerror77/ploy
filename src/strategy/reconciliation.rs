//! Position Reconciliation Service
//!
//! Periodically reconciles local positions with exchange balances:
//! - Detect discrepancies between local DB and exchange
//! - Auto-correct minor differences
//! - Alert on critical mismatches
//! - Track reconciliation history

use crate::adapters::{PolymarketClient, PostgresStore};
use crate::error::Result;
use crate::strategy::position_manager::PositionManager;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, Instant};
use tracing::{debug, error, info, warn};

/// Discrepancy severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiscrepancySeverity {
    /// Minor difference (< 5%)
    Info,
    /// Moderate difference (5-20%)
    Warning,
    /// Major difference (> 20%)
    Critical,
}

impl std::fmt::Display for DiscrepancySeverity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscrepancySeverity::Info => write!(f, "INFO"),
            DiscrepancySeverity::Warning => write!(f, "WARNING"),
            DiscrepancySeverity::Critical => write!(f, "CRITICAL"),
        }
    }
}

/// Position discrepancy record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionDiscrepancy {
    pub token_id: String,
    pub local_shares: i64,
    pub exchange_shares: i64,
    pub difference: i64,
    pub severity: DiscrepancySeverity,
}

/// Reconciliation result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationResult {
    pub timestamp: DateTime<Utc>,
    pub discrepancies_found: usize,
    pub auto_corrections: usize,
    pub critical_issues: usize,
    pub duration_ms: u64,
    pub discrepancies: Vec<PositionDiscrepancy>,
}

/// Reconciliation configuration
#[derive(Debug, Clone)]
pub struct ReconciliationConfig {
    /// Reconciliation interval in seconds (default: 30s)
    pub interval_secs: u64,
    /// Auto-correct threshold (default: 5%)
    pub auto_correct_threshold_pct: Decimal,
    /// Critical threshold (default: 20%)
    pub critical_threshold_pct: Decimal,
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        use rust_decimal_macros::dec;
        Self {
            interval_secs: 30,
            auto_correct_threshold_pct: dec!(0.05), // 5%
            critical_threshold_pct: dec!(0.20),      // 20%
        }
    }
}

/// Position reconciliation service
pub struct ReconciliationService {
    position_manager: Arc<PositionManager>,
    client: Arc<PolymarketClient>,
    store: Arc<PostgresStore>,
    config: ReconciliationConfig,
}

impl ReconciliationService {
    /// Create a new reconciliation service
    pub fn new(
        position_manager: Arc<PositionManager>,
        client: Arc<PolymarketClient>,
        store: Arc<PostgresStore>,
        config: ReconciliationConfig,
    ) -> Self {
        Self {
            position_manager,
            client,
            store,
            config,
        }
    }

    /// Run reconciliation service in background
    ///
    /// This will run indefinitely, performing reconciliation at the configured interval.
    pub async fn run(&self) -> Result<()> {
        let mut ticker = interval(Duration::from_secs(self.config.interval_secs));

        info!(
            "Starting reconciliation service (interval: {}s)",
            self.config.interval_secs
        );

        loop {
            ticker.tick().await;

            match self.reconcile().await {
                Ok(result) => {
                    info!(
                        "Reconciliation completed: {} discrepancies, {} auto-corrected, {} critical ({}ms)",
                        result.discrepancies_found,
                        result.auto_corrections,
                        result.critical_issues,
                        result.duration_ms
                    );

                    // Log critical issues
                    for disc in &result.discrepancies {
                        if disc.severity == DiscrepancySeverity::Critical {
                            error!(
                                "CRITICAL: Position mismatch for {}: local={}, exchange={}, diff={}",
                                &disc.token_id[..16.min(disc.token_id.len())],
                                disc.local_shares,
                                disc.exchange_shares,
                                disc.difference
                            );
                        }
                    }
                }
                Err(e) => {
                    error!("Reconciliation failed: {}", e);
                }
            }
        }
    }

    /// Perform a single reconciliation cycle
    pub async fn reconcile(&self) -> Result<ReconciliationResult> {
        let start = Instant::now();

        // Get all open positions from local DB
        let local_positions = self.position_manager.get_open_positions().await?;

        // Build local position map: token_id -> shares
        let mut local_map: HashMap<String, i64> = HashMap::new();
        for pos in &local_positions {
            *local_map.entry(pos.token_id.clone()).or_insert(0) += pos.shares;
        }

        // Get exchange balances
        let exchange_balances = self.get_exchange_balances().await?;

        // Compare and detect discrepancies
        let mut discrepancies = Vec::new();
        let mut auto_corrections = 0;
        let mut critical_issues = 0;

        // Check all tokens (union of local and exchange)
        let all_tokens: std::collections::HashSet<_> = local_map
            .keys()
            .chain(exchange_balances.keys())
            .cloned()
            .collect();

        for token_id in all_tokens {
            let local_shares = *local_map.get(&token_id).unwrap_or(&0);
            let exchange_shares = *exchange_balances.get(&token_id).unwrap_or(&0);
            let difference = local_shares - exchange_shares;

            if difference != 0 {
                // Calculate severity
                let severity = self.calculate_severity(local_shares, exchange_shares);

                discrepancies.push(PositionDiscrepancy {
                    token_id: token_id.clone(),
                    local_shares,
                    exchange_shares,
                    difference,
                    severity,
                });

                // Auto-correct if within threshold
                if severity == DiscrepancySeverity::Info {
                    match self.auto_correct(&token_id, exchange_shares).await {
                        Ok(()) => {
                            auto_corrections += 1;
                            info!(
                                "Auto-corrected position for {}: {} -> {}",
                                &token_id[..16.min(token_id.len())],
                                local_shares,
                                exchange_shares
                            );
                        }
                        Err(e) => {
                            warn!("Failed to auto-correct position for {}: {}", token_id, e);
                        }
                    }
                } else if severity == DiscrepancySeverity::Critical {
                    critical_issues += 1;
                }

                // Record discrepancy in database
                if let Err(e) = self.record_discrepancy(&token_id, local_shares, exchange_shares, severity).await {
                    warn!("Failed to record discrepancy: {}", e);
                }
            }
        }

        let duration_ms = start.elapsed().as_millis() as u64;

        // Record reconciliation result
        let result = ReconciliationResult {
            timestamp: Utc::now(),
            discrepancies_found: discrepancies.len(),
            auto_corrections,
            critical_issues,
            duration_ms,
            discrepancies,
        };

        self.record_reconciliation(&result).await?;

        Ok(result)
    }

    /// Get exchange balances for all tokens
    async fn get_exchange_balances(&self) -> Result<HashMap<String, i64>> {
        // Get all positions from exchange
        let positions = self.client.get_positions().await?;

        let mut balances = HashMap::new();
        for pos in positions {
            // Parse size as i64
            if let Ok(size) = pos.size.parse::<i64>() {
                // Only count non-zero positions
                if size > 0 {
                    balances.insert(pos.asset_id.clone(), size);
                }
            }
        }

        debug!("Fetched {} exchange positions", balances.len());
        Ok(balances)
    }

    /// Calculate discrepancy severity
    fn calculate_severity(&self, local_shares: i64, exchange_shares: i64) -> DiscrepancySeverity {
        if exchange_shares == 0 {
            // If exchange has 0 but local has something, it's critical
            if local_shares > 0 {
                return DiscrepancySeverity::Critical;
            } else {
                return DiscrepancySeverity::Info;
            }
        }

        let diff_pct = Decimal::from(local_shares.abs_diff(exchange_shares))
            / Decimal::from(exchange_shares.abs());

        if diff_pct >= self.config.critical_threshold_pct {
            DiscrepancySeverity::Critical
        } else if diff_pct >= self.config.auto_correct_threshold_pct {
            DiscrepancySeverity::Warning
        } else {
            DiscrepancySeverity::Info
        }
    }

    /// Auto-correct a position discrepancy
    async fn auto_correct(&self, token_id: &str, correct_shares: i64) -> Result<()> {
        // Update local position to match exchange
        sqlx::query(
            r#"
            UPDATE positions
            SET shares = $1
            WHERE token_id = $2 AND status = 'OPEN'
            "#,
        )
        .bind(correct_shares)
        .bind(token_id)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }

    /// Record a discrepancy in the database
    async fn record_discrepancy(
        &self,
        token_id: &str,
        local_shares: i64,
        exchange_shares: i64,
        severity: DiscrepancySeverity,
    ) -> Result<()> {
        let difference = local_shares - exchange_shares;
        let severity_str = severity.to_string();

        sqlx::query(
            r#"
            INSERT INTO position_discrepancies (
                token_id, local_shares, exchange_shares, difference, severity
            )
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(token_id)
        .bind(local_shares)
        .bind(exchange_shares)
        .bind(difference)
        .bind(severity_str)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }

    /// Record reconciliation result in database
    async fn record_reconciliation(&self, result: &ReconciliationResult) -> Result<()> {
        let details = serde_json::to_value(&result.discrepancies)?;

        sqlx::query(
            r#"
            INSERT INTO position_reconciliation_log (
                discrepancies_found, auto_corrections, details, duration_ms
            )
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(result.discrepancies_found as i32)
        .bind(result.auto_corrections as i32)
        .bind(details)
        .bind(result.duration_ms as i32)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }

    /// Get recent reconciliation history
    pub async fn get_recent_reconciliations(&self, limit: i32) -> Result<Vec<ReconciliationResult>> {
        let rows = sqlx::query_as::<_, (DateTime<Utc>, i32, i32, Option<serde_json::Value>, Option<i32>)>(
            r#"
            SELECT timestamp, discrepancies_found, auto_corrections, details, duration_ms
            FROM position_reconciliation_log
            ORDER BY timestamp DESC
            LIMIT $1
            "#,
        )
        .bind(limit)
        .fetch_all(self.store.pool())
        .await?;

        let mut results = Vec::new();
        for row in rows {
            let discrepancies = if let Some(details) = row.3 {
                serde_json::from_value(details).unwrap_or_default()
            } else {
                Vec::new()
            };

            let critical_issues = discrepancies
                .iter()
                .filter(|d: &&PositionDiscrepancy| d.severity == DiscrepancySeverity::Critical)
                .count();

            results.push(ReconciliationResult {
                timestamp: row.0,
                discrepancies_found: row.1 as usize,
                auto_corrections: row.2 as usize,
                critical_issues,
                duration_ms: row.4.unwrap_or(0) as u64,
                discrepancies,
            });
        }

        Ok(results)
    }

    /// Get unresolved discrepancies
    pub async fn get_unresolved_discrepancies(&self) -> Result<Vec<PositionDiscrepancy>> {
        let rows = sqlx::query_as::<_, (String, i64, i64, i64, String)>(
            r#"
            SELECT token_id, local_shares, exchange_shares, difference, severity
            FROM position_discrepancies
            WHERE resolved = FALSE
            ORDER BY severity DESC, created_at ASC
            "#,
        )
        .fetch_all(self.store.pool())
        .await?;

        let mut discrepancies = Vec::new();
        for row in rows {
            let severity = match row.4.as_str() {
                "CRITICAL" => DiscrepancySeverity::Critical,
                "WARNING" => DiscrepancySeverity::Warning,
                _ => DiscrepancySeverity::Info,
            };

            discrepancies.push(PositionDiscrepancy {
                token_id: row.0,
                local_shares: row.1,
                exchange_shares: row.2,
                difference: row.3,
                severity,
            });
        }

        Ok(discrepancies)
    }

    /// Mark a discrepancy as resolved
    pub async fn mark_discrepancy_resolved(&self, token_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE position_discrepancies
            SET resolved = TRUE, resolved_at = NOW()
            WHERE token_id = $1 AND resolved = FALSE
            "#,
        )
        .bind(token_id)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn test_severity_thresholds() {
        let config = ReconciliationConfig::default();

        // Test INFO severity (< 5% difference)
        // 2% difference: 100 vs 102
        let local: i64 = 100;
        let exchange: i64 = 102;
        let diff_pct = Decimal::from(local.abs_diff(exchange)) / Decimal::from(exchange.abs());
        assert!(diff_pct < config.auto_correct_threshold_pct);

        // Test WARNING severity (5-20% difference)
        // 10% difference: 100 vs 110
        let local: i64 = 100;
        let exchange: i64 = 110;
        let diff_pct = Decimal::from(local.abs_diff(exchange)) / Decimal::from(exchange.abs());
        assert!(diff_pct >= config.auto_correct_threshold_pct);
        assert!(diff_pct < config.critical_threshold_pct);

        // Test CRITICAL severity (> 20% difference)
        // 30% difference: 100 vs 130
        let local: i64 = 100;
        let exchange: i64 = 130;
        let diff_pct = Decimal::from(local.abs_diff(exchange)) / Decimal::from(exchange.abs());
        assert!(diff_pct >= config.critical_threshold_pct);
    }

    // Note: Integration tests require database and exchange client
    // Run with: cargo test --features test-integration
}
