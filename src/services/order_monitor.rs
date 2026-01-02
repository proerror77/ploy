//! Order monitoring background service
//!
//! This service periodically monitors orders and performs:
//! - Orphaned order detection and cleanup
//! - Order status reconciliation with exchange
//! - Position tracking updates

use crate::adapters::{PostgresStore, PolymarketClient};
use crate::domain::OrderStatus;
use crate::error::Result;
use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Configuration for the order monitor
#[derive(Debug, Clone)]
pub struct OrderMonitorConfig {
    /// Interval between monitoring cycles (seconds)
    pub check_interval_secs: u64,
    /// Age threshold for orphaned orders (seconds)
    pub orphan_threshold_secs: u64,
    /// Whether to auto-cancel orphaned orders
    pub auto_cancel_orphans: bool,
    /// Maximum orders to check per cycle
    pub max_orders_per_cycle: usize,
}

impl Default for OrderMonitorConfig {
    fn default() -> Self {
        Self {
            check_interval_secs: 30,
            orphan_threshold_secs: 300, // 5 minutes
            auto_cancel_orphans: true,
            max_orders_per_cycle: 50,
        }
    }
}

/// Tracked order for monitoring
#[derive(Debug, Clone)]
pub struct TrackedOrder {
    pub client_order_id: String,
    pub exchange_order_id: Option<String>,
    pub token_id: String,
    pub side: String,
    pub shares: u64,
    pub limit_price: Decimal,
    pub status: OrderStatus,
    pub submitted_at: DateTime<Utc>,
    pub last_checked: DateTime<Utc>,
    pub check_count: u32,
}

/// Order monitoring statistics
#[derive(Debug, Clone, Default)]
pub struct MonitorStats {
    pub orders_checked: u64,
    pub orders_filled: u64,
    pub orders_cancelled: u64,
    pub orders_orphaned: u64,
    pub reconciliation_errors: u64,
    pub last_check: Option<DateTime<Utc>>,
}

/// Order monitoring service
pub struct OrderMonitor {
    client: Arc<PolymarketClient>,
    store: Option<Arc<PostgresStore>>,
    config: OrderMonitorConfig,
    /// Orders being tracked
    tracked_orders: Arc<RwLock<HashMap<String, TrackedOrder>>>,
    /// Running flag
    running: Arc<AtomicBool>,
    /// Statistics
    stats: Arc<RwLock<MonitorStats>>,
}

impl OrderMonitor {
    /// Create a new order monitor
    pub fn new(
        client: Arc<PolymarketClient>,
        store: Option<Arc<PostgresStore>>,
        config: OrderMonitorConfig,
    ) -> Self {
        Self {
            client,
            store,
            config,
            tracked_orders: Arc::new(RwLock::new(HashMap::new())),
            running: Arc::new(AtomicBool::new(false)),
            stats: Arc::new(RwLock::new(MonitorStats::default())),
        }
    }

    /// Add an order to track
    pub async fn track_order(&self, order: TrackedOrder) {
        let mut orders = self.tracked_orders.write().await;
        info!(
            "Tracking order {} (exchange: {:?})",
            order.client_order_id, order.exchange_order_id
        );
        orders.insert(order.client_order_id.clone(), order);
    }

    /// Remove an order from tracking
    pub async fn untrack_order(&self, client_order_id: &str) {
        let mut orders = self.tracked_orders.write().await;
        if orders.remove(client_order_id).is_some() {
            debug!("Untracked order {}", client_order_id);
        }
    }

    /// Get current statistics
    pub async fn get_stats(&self) -> MonitorStats {
        self.stats.read().await.clone()
    }

    /// Get count of tracked orders
    pub async fn tracked_count(&self) -> usize {
        self.tracked_orders.read().await.len()
    }

    /// Start the monitoring loop
    pub async fn start(&self) {
        if self.running.swap(true, Ordering::SeqCst) {
            warn!("Order monitor already running");
            return;
        }

        info!(
            "Starting order monitor (interval: {}s, orphan threshold: {}s)",
            self.config.check_interval_secs, self.config.orphan_threshold_secs
        );

        let client = self.client.clone();
        let store = self.store.clone();
        let config = self.config.clone();
        let tracked_orders = self.tracked_orders.clone();
        let running = self.running.clone();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(tokio::time::Duration::from_secs(config.check_interval_secs));

            while running.load(Ordering::SeqCst) {
                interval.tick().await;

                if let Err(e) = Self::run_check_cycle(
                    &client,
                    store.as_ref().map(|s| s.as_ref()),
                    &config,
                    &tracked_orders,
                    &stats,
                )
                .await
                {
                    error!("Order monitor check failed: {}", e);
                }
            }

            info!("Order monitor stopped");
        });
    }

    /// Stop the monitoring loop
    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        info!("Order monitor stop requested");
    }

    /// Run a single check cycle
    async fn run_check_cycle(
        client: &PolymarketClient,
        store: Option<&PostgresStore>,
        config: &OrderMonitorConfig,
        tracked_orders: &RwLock<HashMap<String, TrackedOrder>>,
        stats: &RwLock<MonitorStats>,
    ) -> Result<()> {
        let now = Utc::now();
        let orphan_threshold = Duration::seconds(config.orphan_threshold_secs as i64);

        // Get orders to check
        let orders_to_check: Vec<TrackedOrder> = {
            let orders = tracked_orders.read().await;
            orders
                .values()
                .filter(|o| {
                    matches!(
                        o.status,
                        OrderStatus::Pending | OrderStatus::Submitted | OrderStatus::PartiallyFilled
                    )
                })
                .take(config.max_orders_per_cycle)
                .cloned()
                .collect()
        };

        if orders_to_check.is_empty() {
            debug!("No orders to check");
            return Ok(());
        }

        debug!("Checking {} tracked orders", orders_to_check.len());

        let mut checked = 0u64;
        let mut filled = 0u64;
        let mut cancelled = 0u64;
        let mut orphaned = 0u64;
        let mut errors = 0u64;

        for order in orders_to_check {
            checked += 1;

            // Check if order is orphaned
            let age = now - order.submitted_at;
            if age > orphan_threshold {
                orphaned += 1;
                warn!(
                    "Orphaned order detected: {} (age: {}s)",
                    order.client_order_id,
                    age.num_seconds()
                );

                if config.auto_cancel_orphans {
                    if let Some(exchange_id) = &order.exchange_order_id {
                        match client.cancel_order(exchange_id).await {
                            Ok(true) => {
                                info!("Auto-cancelled orphaned order: {}", order.client_order_id);
                                cancelled += 1;

                                // Update database
                                if let Some(store) = store {
                                    let _ = store
                                        .mark_order_cancelled(
                                            &order.client_order_id,
                                            "Auto-cancelled by order monitor (orphaned)",
                                        )
                                        .await;
                                }

                                // Remove from tracking
                                let mut orders = tracked_orders.write().await;
                                orders.remove(&order.client_order_id);
                            }
                            Ok(false) => {
                                warn!(
                                    "Failed to cancel orphaned order: {} (already cancelled?)",
                                    order.client_order_id
                                );
                            }
                            Err(e) => {
                                error!(
                                    "Error cancelling orphaned order {}: {}",
                                    order.client_order_id, e
                                );
                                errors += 1;
                            }
                        }
                    }
                }
                continue;
            }

            // Check order status from exchange
            if let Some(exchange_id) = &order.exchange_order_id {
                match client.get_order(exchange_id).await {
                    Ok(response) => {
                        let new_status = PolymarketClient::parse_order_status(&response.status);

                        if new_status != order.status {
                            info!(
                                "Order {} status changed: {:?} -> {:?}",
                                order.client_order_id, order.status, new_status
                            );

                            // Update tracked order
                            {
                                let mut orders = tracked_orders.write().await;
                                if let Some(tracked) = orders.get_mut(&order.client_order_id) {
                                    tracked.status = new_status.clone();
                                    tracked.last_checked = now;
                                    tracked.check_count += 1;
                                }
                            }

                            // Update database
                            if let Some(store) = store {
                                let _ = store
                                    .update_order_status(&order.client_order_id, new_status.clone(), None)
                                    .await;

                                // If filled, update fill info
                                if new_status == OrderStatus::Filled {
                                    let (filled_shares, avg_price) =
                                        PolymarketClient::calculate_fill(&response);
                                    if let Some(price) = avg_price {
                                        let _ = store
                                            .update_order_fill(
                                                &order.client_order_id,
                                                filled_shares,
                                                price,
                                                OrderStatus::Filled,
                                            )
                                            .await;
                                    }
                                    filled += 1;
                                }
                            }

                            // Remove from tracking if terminal state
                            if matches!(
                                new_status,
                                OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Expired
                            ) {
                                let mut orders = tracked_orders.write().await;
                                orders.remove(&order.client_order_id);
                            }
                        } else {
                            // Just update last checked time
                            let mut orders = tracked_orders.write().await;
                            if let Some(tracked) = orders.get_mut(&order.client_order_id) {
                                tracked.last_checked = now;
                                tracked.check_count += 1;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Failed to get order status for {}: {}",
                            order.client_order_id, e
                        );
                        errors += 1;
                    }
                }
            }
        }

        // Update stats
        {
            let mut s = stats.write().await;
            s.orders_checked += checked;
            s.orders_filled += filled;
            s.orders_cancelled += cancelled;
            s.orders_orphaned += orphaned;
            s.reconciliation_errors += errors;
            s.last_check = Some(now);
        }

        debug!(
            "Monitor cycle complete: checked={}, filled={}, cancelled={}, orphaned={}, errors={}",
            checked, filled, cancelled, orphaned, errors
        );

        Ok(())
    }

    /// Reconcile orders with exchange (on startup or periodic)
    pub async fn reconcile_with_exchange(&self) -> Result<ReconciliationResult> {
        info!("Starting order reconciliation with exchange...");

        let mut result = ReconciliationResult::default();

        // Get open orders from exchange
        let exchange_orders = self.client.get_open_orders().await?;
        result.exchange_order_count = exchange_orders.len();

        // Get tracked orders
        let tracked = self.tracked_orders.read().await;
        result.tracked_order_count = tracked.len();

        // Build lookup map
        let exchange_order_map: HashMap<_, _> = exchange_orders
            .iter()
            .map(|o| (o.id.clone(), o))
            .collect();

        // Check each tracked order
        for (client_id, tracked_order) in tracked.iter() {
            if let Some(exchange_id) = &tracked_order.exchange_order_id {
                if let Some(exchange_order) = exchange_order_map.get(exchange_id) {
                    // Order exists on exchange, check status
                    let exchange_status =
                        PolymarketClient::parse_order_status(&exchange_order.status);
                    if exchange_status != tracked_order.status {
                        result.status_mismatches.push((
                            client_id.clone(),
                            tracked_order.status.clone(),
                            exchange_status,
                        ));
                    }
                } else {
                    // Order not found on exchange (might be filled/cancelled)
                    result.missing_from_exchange.push(client_id.clone());
                }
            }
        }

        // Find orders on exchange not being tracked
        let tracked_exchange_ids: std::collections::HashSet<_> = tracked
            .values()
            .filter_map(|o| o.exchange_order_id.clone())
            .collect();

        for exchange_order in &exchange_orders {
            if !tracked_exchange_ids.contains(&exchange_order.id) {
                result.untracked_exchange_orders.push(exchange_order.id.clone());
            }
        }

        info!(
            "Reconciliation complete: exchange={}, tracked={}, mismatches={}, missing={}, untracked={}",
            result.exchange_order_count,
            result.tracked_order_count,
            result.status_mismatches.len(),
            result.missing_from_exchange.len(),
            result.untracked_exchange_orders.len()
        );

        Ok(result)
    }

    /// Log current monitor status
    pub async fn log_status(&self) {
        let stats = self.stats.read().await;
        let tracked_count = self.tracked_orders.read().await.len();

        info!(
            "Order Monitor Status: tracking={}, checked={}, filled={}, cancelled={}, orphaned={}, errors={}, last_check={:?}",
            tracked_count,
            stats.orders_checked,
            stats.orders_filled,
            stats.orders_cancelled,
            stats.orders_orphaned,
            stats.reconciliation_errors,
            stats.last_check
        );
    }
}

/// Result of order reconciliation
#[derive(Debug, Clone, Default)]
pub struct ReconciliationResult {
    pub exchange_order_count: usize,
    pub tracked_order_count: usize,
    pub status_mismatches: Vec<(String, OrderStatus, OrderStatus)>,
    pub missing_from_exchange: Vec<String>,
    pub untracked_exchange_orders: Vec<String>,
}

impl ReconciliationResult {
    /// Check if reconciliation found any issues
    pub fn has_issues(&self) -> bool {
        !self.status_mismatches.is_empty()
            || !self.missing_from_exchange.is_empty()
            || !self.untracked_exchange_orders.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OrderMonitorConfig::default();
        assert_eq!(config.check_interval_secs, 30);
        assert_eq!(config.orphan_threshold_secs, 300);
        assert!(config.auto_cancel_orphans);
    }
}
