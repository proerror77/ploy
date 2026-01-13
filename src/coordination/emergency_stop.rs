//! Emergency Stop Mechanism
//!
//! Provides emergency shutdown capabilities for critical situations:
//! - Immediate trading halt
//! - Cancel all pending orders
//! - Close open positions (optional)
//! - Persist emergency state
//! - Prevent new operations

use crate::adapters::{PolymarketClient, PostgresStore};
use crate::error::{PloyError, Result};
use crate::strategy::position_manager::PositionManager;
use crate::strategy::trading_costs::OrderType;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use sqlx::types::chrono::{DateTime, Utc};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// Emergency stop reason
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmergencyReason {
    /// Manual trigger by operator
    Manual,
    /// Circuit breaker triggered
    CircuitBreaker,
    /// Critical position discrepancy
    PositionDiscrepancy,
    /// Exchange connectivity issues
    ExchangeConnectivity,
    /// Risk limit exceeded
    RiskLimitExceeded,
    /// Database failure
    DatabaseFailure,
    /// Unknown/other reason
    Other(String),
}

impl std::fmt::Display for EmergencyReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EmergencyReason::Manual => write!(f, "Manual"),
            EmergencyReason::CircuitBreaker => write!(f, "CircuitBreaker"),
            EmergencyReason::PositionDiscrepancy => write!(f, "PositionDiscrepancy"),
            EmergencyReason::ExchangeConnectivity => write!(f, "ExchangeConnectivity"),
            EmergencyReason::RiskLimitExceeded => write!(f, "RiskLimitExceeded"),
            EmergencyReason::DatabaseFailure => write!(f, "DatabaseFailure"),
            EmergencyReason::Other(s) => write!(f, "Other: {}", s),
        }
    }
}

/// Emergency stop state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmergencyState {
    /// Whether emergency stop is active
    pub active: bool,
    /// Reason for emergency stop
    pub reason: Option<EmergencyReason>,
    /// Timestamp when emergency stop was triggered
    pub triggered_at: Option<DateTime<Utc>>,
    /// Additional context
    pub context: Option<String>,
}

impl Default for EmergencyState {
    fn default() -> Self {
        Self {
            active: false,
            reason: None,
            triggered_at: None,
            context: None,
        }
    }
}

/// Emergency stop configuration
#[derive(Debug, Clone)]
pub struct EmergencyStopConfig {
    /// Whether to cancel all pending orders on emergency stop
    pub cancel_pending_orders: bool,
    /// Whether to close all open positions on emergency stop
    pub close_open_positions: bool,
    /// Maximum time to wait for order cancellations (seconds)
    pub cancel_timeout_secs: u64,
    /// Maximum time to wait for position closures (seconds)
    pub close_timeout_secs: u64,
}

impl Default for EmergencyStopConfig {
    fn default() -> Self {
        Self {
            cancel_pending_orders: true,
            close_open_positions: false, // Don't auto-close by default (too risky)
            cancel_timeout_secs: 30,
            close_timeout_secs: 60,
        }
    }
}

/// Emergency stop manager
pub struct EmergencyStopManager {
    state: Arc<RwLock<EmergencyState>>,
    is_stopped: Arc<AtomicBool>,
    client: Arc<PolymarketClient>,
    position_manager: Arc<PositionManager>,
    store: Arc<PostgresStore>,
    config: EmergencyStopConfig,
}

impl EmergencyStopManager {
    /// Create a new emergency stop manager
    pub fn new(
        client: Arc<PolymarketClient>,
        position_manager: Arc<PositionManager>,
        store: Arc<PostgresStore>,
        config: EmergencyStopConfig,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(EmergencyState::default())),
            is_stopped: Arc::new(AtomicBool::new(false)),
            client,
            position_manager,
            store,
            config,
        }
    }

    /// Check if emergency stop is active (fast atomic check)
    pub fn is_stopped(&self) -> bool {
        self.is_stopped.load(Ordering::Relaxed)
    }

    /// Get current emergency state
    pub async fn get_state(&self) -> EmergencyState {
        self.state.read().await.clone()
    }

    /// Trigger emergency stop
    ///
    /// This will:
    /// 1. Set emergency stop flag
    /// 2. Cancel pending orders (if configured)
    /// 3. Close open positions (if configured)
    /// 4. Persist state to database
    pub async fn trigger(&self, reason: EmergencyReason, context: Option<String>) -> Result<()> {
        error!("🚨 EMERGENCY STOP TRIGGERED: {} - {:?}", reason, context);

        // Set atomic flag immediately
        self.is_stopped.store(true, Ordering::SeqCst);

        // Update state
        let mut state = self.state.write().await;
        state.active = true;
        state.reason = Some(reason.clone());
        state.triggered_at = Some(Utc::now());
        state.context = context.clone();
        drop(state);

        // Persist to database
        self.persist_emergency_state(&reason, context.as_deref()).await?;

        // Cancel pending orders if configured
        if self.config.cancel_pending_orders {
            info!("Cancelling all pending orders...");
            match self.cancel_all_orders().await {
                Ok(count) => info!("Cancelled {} pending orders", count),
                Err(e) => error!("Failed to cancel pending orders: {}", e),
            }
        }

        // Close open positions if configured
        if self.config.close_open_positions {
            warn!("Closing all open positions...");
            match self.close_all_positions().await {
                Ok(count) => info!("Closed {} open positions", count),
                Err(e) => error!("Failed to close open positions: {}", e),
            }
        }

        info!("Emergency stop completed");
        Ok(())
    }

    /// Reset emergency stop (requires manual intervention)
    pub async fn reset(&self, operator: &str) -> Result<()> {
        info!("Resetting emergency stop (operator: {})", operator);

        // Clear atomic flag
        self.is_stopped.store(false, Ordering::SeqCst);

        // Update state
        let mut state = self.state.write().await;
        *state = EmergencyState::default();
        drop(state);

        // Record reset in database
        sqlx::query(
            r#"
            INSERT INTO system_events (event_type, severity, message, metadata)
            VALUES ('emergency_stop_reset', 'INFO', 'Emergency stop reset', $1)
            "#,
        )
        .bind(serde_json::json!({ "operator": operator }))
        .execute(self.store.pool())
        .await?;

        info!("Emergency stop reset complete");
        Ok(())
    }

    /// Check if operation should be allowed
    ///
    /// Returns Err if emergency stop is active
    pub fn check_allowed(&self) -> Result<()> {
        if self.is_stopped() {
            Err(PloyError::CircuitBreakerTriggered(
                "Emergency stop is active - all trading operations are blocked".to_string()
            ))
        } else {
            Ok(())
        }
    }

    /// Cancel all pending orders
    async fn cancel_all_orders(&self) -> Result<usize> {
        // Get all open orders from exchange
        let orders = self.client.get_open_orders().await?;

        let mut cancelled = 0;
        for order in orders {
            match self.client.cancel_order(&order.id).await {
                Ok(true) => {
                    cancelled += 1;
                    info!("Cancelled order: {}", order.id);
                }
                Ok(false) => {
                    warn!("Order {} already cancelled or filled", order.id);
                }
                Err(e) => {
                    error!("Failed to cancel order {}: {}", order.id, e);
                }
            }
        }

        Ok(cancelled)
    }

    /// Close all open positions
    async fn close_all_positions(&self) -> Result<usize> {
        // Get all open positions
        let positions = self.position_manager.get_open_positions().await?;

        let mut closed = 0;
        for pos in positions {
            // Get current market price
            match self.client.get_best_prices(&pos.token_id).await {
                Ok((Some(bid), _)) => {
                    // Close at current bid price (taker order, assume 2% market depth)
                    match self.position_manager.close_position(
                        pos.id,
                        bid,
                        OrderType::Taker,
                        dec!(0.02), // 2% of market depth
                    ).await {
                        Ok(pnl) => {
                            closed += 1;
                            info!(
                                "Closed position #{} for {} (PnL: ${:.2})",
                                pos.id, pos.symbol, pnl
                            );
                        }
                        Err(e) => {
                            error!("Failed to close position #{}: {}", pos.id, e);
                        }
                    }
                }
                Ok((None, _)) => {
                    warn!("No bid price available for position #{}", pos.id);
                }
                Err(e) => {
                    error!("Failed to get price for position #{}: {}", pos.id, e);
                }
            }
        }

        Ok(closed)
    }

    /// Persist emergency state to database
    async fn persist_emergency_state(&self, reason: &EmergencyReason, context: Option<&str>) -> Result<()> {
        let reason_str = reason.to_string();
        let metadata = serde_json::json!({
            "reason": reason_str,
            "context": context,
        });

        sqlx::query(
            r#"
            INSERT INTO system_events (event_type, severity, message, metadata)
            VALUES ('emergency_stop', 'CRITICAL', $1, $2)
            "#,
        )
        .bind(format!("Emergency stop triggered: {}", reason_str))
        .bind(metadata)
        .execute(self.store.pool())
        .await?;

        Ok(())
    }

    /// Load emergency state from database on startup
    pub async fn load_state(&self) -> Result<()> {
        // Check if there's an active emergency stop in the database
        let row: Option<(String, Option<String>)> = sqlx::query_as(
            r#"
            SELECT message, metadata
            FROM system_events
            WHERE event_type = 'emergency_stop'
            ORDER BY timestamp DESC
            LIMIT 1
            "#,
        )
        .fetch_optional(self.store.pool())
        .await?;

        if let Some((message, metadata)) = row {
            // Check if it was reset
            let reset_exists: bool = sqlx::query_scalar(
                r#"
                SELECT EXISTS(
                    SELECT 1 FROM system_events
                    WHERE event_type = 'emergency_stop_reset'
                    AND timestamp > (
                        SELECT timestamp FROM system_events
                        WHERE event_type = 'emergency_stop'
                        ORDER BY timestamp DESC
                        LIMIT 1
                    )
                )
                "#,
            )
            .fetch_one(self.store.pool())
            .await?;

            if !reset_exists {
                warn!("Found active emergency stop from previous session: {}", message);
                self.is_stopped.store(true, Ordering::SeqCst);

                let mut state = self.state.write().await;
                state.active = true;
                state.reason = Some(EmergencyReason::Other("Loaded from database".to_string()));
                state.context = metadata;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_emergency_reason_display() {
        assert_eq!(EmergencyReason::Manual.to_string(), "Manual");
        assert_eq!(EmergencyReason::CircuitBreaker.to_string(), "CircuitBreaker");
        assert_eq!(
            EmergencyReason::Other("test".to_string()).to_string(),
            "Other: test"
        );
    }

    #[test]
    fn test_default_state() {
        let state = EmergencyState::default();
        assert!(!state.active);
        assert!(state.reason.is_none());
        assert!(state.triggered_at.is_none());
    }

    // Note: Integration tests require database and exchange client
    // Run with: cargo test --features test-integration
}
