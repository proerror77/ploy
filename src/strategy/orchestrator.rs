//! Strategy Orchestrator
//!
//! Manages multiple strategies, routes market updates, and executes actions.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use crate::error::Result;

use super::core::{ExecutionConfig, OrderExecutor, PositionManager, RiskCheck, RiskConfig, RiskManager};
use super::multi_event::MultiEventMonitor;
use super::reconciliation::ReconciliationService;
use super::traits::{
    DataFeed, MarketUpdate, OrderUpdate, Strategy, StrategyAction, StrategyStateInfo,
};

/// Orchestrator configuration
#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Tick interval for strategies (ms)
    pub tick_interval_ms: u64,
    /// Maximum concurrent strategies
    pub max_strategies: usize,
    /// Enable dry run mode
    pub dry_run: bool,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            tick_interval_ms: 1000,
            max_strategies: 10,
            dry_run: true,
        }
    }
}

/// Strategy orchestrator
pub struct StrategyOrchestrator {
    config: OrchestratorConfig,
    strategies: Arc<RwLock<HashMap<String, Box<dyn Strategy>>>>,
    executor: Arc<OrderExecutor>,
    risk_manager: Arc<RiskManager>,
    position_manager: Arc<PositionManager>,
    active_feeds: Arc<RwLock<HashMap<DataFeed, Vec<String>>>>,
    shutdown: Arc<RwLock<bool>>,
    /// Optional reconciliation service for periodic position checks
    reconciliation: Option<Arc<ReconciliationService>>,
    /// Optional multi-event monitor for cross-event arbitrage scanning
    multi_event_monitor: Option<Arc<RwLock<MultiEventMonitor>>>,
}

impl StrategyOrchestrator {
    /// Create a new orchestrator
    pub fn new(
        config: OrchestratorConfig,
        executor: Arc<OrderExecutor>,
        risk_manager: Arc<RiskManager>,
        position_manager: Arc<PositionManager>,
    ) -> Self {
        Self {
            config,
            strategies: Arc::new(RwLock::new(HashMap::new())),
            executor,
            risk_manager,
            position_manager,
            active_feeds: Arc::new(RwLock::new(HashMap::new())),
            shutdown: Arc::new(RwLock::new(false)),
            reconciliation: None,
            multi_event_monitor: None,
        }
    }

    /// Set the multi-event monitor for cross-event arbitrage scanning
    pub fn set_multi_event_monitor(&mut self, monitor: Arc<RwLock<MultiEventMonitor>>) {
        self.multi_event_monitor = Some(monitor);
    }

    /// Spawn the multi-event monitor background task if configured.
    /// Periodically scans all tracked events for the best arbitrage opportunity.
    pub fn spawn_multi_event_scanner(&self) {
        if let Some(monitor) = &self.multi_event_monitor {
            let monitor = monitor.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(5));
                loop {
                    interval.tick().await;
                    let mon = monitor.read().await;
                    if let Some(opp) = mon.find_best_opportunity() {
                        info!(
                            "Multi-event opportunity: {} sum={:.4} profit={:.4}/share",
                            opp.event_slug, opp.sum, opp.profit_per_share,
                        );
                    }
                }
            });
            info!("Multi-event scanner background task spawned (5s interval)");
        }
    }

    /// Set the reconciliation service
    pub fn set_reconciliation(&mut self, service: Arc<ReconciliationService>) {
        self.reconciliation = Some(service);
    }

    /// Spawn the reconciliation background task if configured
    pub fn spawn_reconciliation(&self) {
        if let Some(recon) = &self.reconciliation {
            let recon = recon.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    match recon.reconcile().await {
                        Ok(result) => {
                            if !result.discrepancies.is_empty() {
                                warn!(
                                    "Reconciliation found {} discrepancies",
                                    result.discrepancies.len()
                                );
                            }
                        }
                        Err(e) => error!("Reconciliation failed: {}", e),
                    }
                }
            });
            info!("Reconciliation background task spawned (30s interval)");
        }
    }

    /// Register a strategy
    pub async fn register(&self, strategy: Box<dyn Strategy>) -> Result<()> {
        let id = strategy.id().to_string();

        info!("Registering strategy: {} ({})", strategy.name(), id);

        // Get required feeds
        let feeds = strategy.required_feeds();

        // Register feeds
        {
            let mut active_feeds = self.active_feeds.write().await;
            for feed in &feeds {
                active_feeds
                    .entry(feed.clone())
                    .or_default()
                    .push(id.clone());
            }
        }

        // Add strategy
        self.strategies.write().await.insert(id.clone(), strategy);

        info!("Strategy {} registered with {} feeds", id, feeds.len());

        Ok(())
    }

    /// Unregister a strategy
    pub async fn unregister(&self, strategy_id: &str) -> Result<()> {
        let mut strategies = self.strategies.write().await;

        if let Some(mut strategy) = strategies.remove(strategy_id) {
            // Shutdown strategy
            let actions = strategy.shutdown().await?;
            drop(strategies);

            // Execute shutdown actions
            self.execute_actions(strategy_id, actions).await?;

            // Remove from feed subscriptions
            let mut active_feeds = self.active_feeds.write().await;
            for subscribers in active_feeds.values_mut() {
                subscribers.retain(|id| id != strategy_id);
            }

            info!("Strategy {} unregistered", strategy_id);
        }

        Ok(())
    }

    /// Process a market update
    pub async fn on_market_update(&self, update: &MarketUpdate) -> Result<()> {
        let mut strategies = self.strategies.write().await;

        for (id, strategy) in strategies.iter_mut() {
            match strategy.on_market_update(update).await {
                Ok(actions) => {
                    if !actions.is_empty() {
                        drop(strategies);
                        self.execute_actions(id, actions).await?;
                        strategies = self.strategies.write().await;
                    }
                }
                Err(e) => {
                    error!("Strategy {} error on market update: {}", id, e);
                }
            }
        }

        Ok(())
    }

    /// Process an order update
    pub async fn on_order_update(&self, update: &OrderUpdate) -> Result<()> {
        let mut strategies = self.strategies.write().await;

        for (id, strategy) in strategies.iter_mut() {
            match strategy.on_order_update(update).await {
                Ok(actions) => {
                    if !actions.is_empty() {
                        drop(strategies);
                        self.execute_actions(id, actions).await?;
                        strategies = self.strategies.write().await;
                    }
                }
                Err(e) => {
                    error!("Strategy {} error on order update: {}", id, e);
                }
            }
        }

        Ok(())
    }

    /// Tick all strategies
    pub async fn tick(&self) -> Result<()> {
        let now = Utc::now();
        let mut strategies = self.strategies.write().await;

        for (id, strategy) in strategies.iter_mut() {
            match strategy.on_tick(now).await {
                Ok(actions) => {
                    if !actions.is_empty() {
                        drop(strategies);
                        self.execute_actions(id, actions).await?;
                        strategies = self.strategies.write().await;
                    }
                }
                Err(e) => {
                    error!("Strategy {} error on tick: {}", id, e);
                }
            }
        }

        Ok(())
    }

    /// Execute strategy actions
    async fn execute_actions(&self, strategy_id: &str, actions: Vec<StrategyAction>) -> Result<()> {
        for action in actions {
            match action {
                StrategyAction::SubmitOrder {
                    client_order_id,
                    order,
                    priority,
                } => {
                    // Check risk before submitting
                    let check = self.risk_manager
                        .check_new_position(
                            strategy_id,
                            order.shares,
                            order.limit_price,
                            None,
                        )
                        .await;

                    if !check.passed {
                        warn!(
                            "Order rejected by risk manager: {:?}",
                            check.reason
                        );
                        continue;
                    }

                    // Execute order
                    match self.executor
                        .execute(&order, Some(client_order_id), strategy_id)
                        .await
                    {
                        Ok(result) => {
                            debug!(
                                "Order {} executed: {} shares filled",
                                result.order_id, result.filled_shares
                            );
                        }
                        Err(e) => {
                            error!("Order execution failed: {}", e);
                        }
                    }
                }
                StrategyAction::CancelOrder { order_id } => {
                    if let Err(e) = self.executor.cancel(&order_id).await {
                        warn!("Failed to cancel order {}: {}", order_id, e);
                    }
                }
                StrategyAction::ModifyOrder { order_id, new_price, new_size } => {
                    info!("Modifying order {}: price={:?}, size={:?}", order_id, new_price, new_size);
                    // Cancel existing order first
                    match self.executor.cancel(&order_id).await {
                        Ok(_) => {
                            info!("Cancelled order {} for modification", order_id);
                            // If we have new price/size, we'd need the original order details
                            // to resubmit. For now, log that cancel succeeded.
                            // The strategy should detect the cancellation and resubmit if needed.
                        }
                        Err(e) => {
                            warn!("Failed to cancel order {} for modification: {}", order_id, e);
                        }
                    }
                }
                StrategyAction::UpdateRisk { level, reason } => {
                    info!("Strategy {} risk update: {:?} - {}", strategy_id, level, reason);
                    // Could trigger circuit breaker based on level
                }
                StrategyAction::LogEvent { event } => {
                    info!(
                        "Strategy {} event: {:?} - {}",
                        strategy_id, event.event_type, event.message
                    );
                }
                StrategyAction::Alert { level, message } => {
                    match level {
                        super::traits::AlertLevel::Info => info!("[ALERT] {}: {}", strategy_id, message),
                        super::traits::AlertLevel::Warning => warn!("[ALERT] {}: {}", strategy_id, message),
                        super::traits::AlertLevel::Error => error!("[ALERT] {}: {}", strategy_id, message),
                        super::traits::AlertLevel::Critical => error!("[CRITICAL] {}: {}", strategy_id, message),
                    }
                }
                StrategyAction::SubscribeFeed { feed } => {
                    let mut active_feeds = self.active_feeds.write().await;
                    active_feeds
                        .entry(feed.clone())
                        .or_default()
                        .push(strategy_id.to_string());
                    debug!("Strategy {} subscribed to {:?}", strategy_id, feed);
                }
                StrategyAction::UnsubscribeFeed { feed } => {
                    let mut active_feeds = self.active_feeds.write().await;
                    if let Some(subscribers) = active_feeds.get_mut(&feed) {
                        subscribers.retain(|id| id != strategy_id);
                    }
                    debug!("Strategy {} unsubscribed from {:?}", strategy_id, feed);
                }
            }
        }

        Ok(())
    }

    /// Get all strategy states
    pub async fn get_states(&self) -> Vec<StrategyStateInfo> {
        let strategies = self.strategies.read().await;
        strategies.values().map(|s| s.state()).collect()
    }

    /// Get a specific strategy state
    pub async fn get_state(&self, strategy_id: &str) -> Option<StrategyStateInfo> {
        let strategies = self.strategies.read().await;
        strategies.get(strategy_id).map(|s| s.state())
    }

    /// Get all required feeds
    pub async fn get_required_feeds(&self) -> Vec<DataFeed> {
        self.active_feeds.read().await.keys().cloned().collect()
    }

    /// Signal shutdown
    pub async fn shutdown(&self) -> Result<()> {
        *self.shutdown.write().await = true;

        let strategy_ids: Vec<String> = self.strategies.read().await.keys().cloned().collect();

        for id in strategy_ids {
            if let Err(e) = self.unregister(&id).await {
                error!("Error shutting down strategy {}: {}", id, e);
            }
        }

        info!("Orchestrator shutdown complete");
        Ok(())
    }

    /// Check if shutdown was requested
    pub async fn is_shutdown(&self) -> bool {
        *self.shutdown.read().await
    }

    /// Get strategy count
    pub async fn strategy_count(&self) -> usize {
        self.strategies.read().await.len()
    }

    /// Check if any strategy is active
    pub async fn has_active_strategies(&self) -> bool {
        let strategies = self.strategies.read().await;
        strategies.values().any(|s| s.is_active())
    }

    /// Get executor reference
    pub fn executor(&self) -> &Arc<OrderExecutor> {
        &self.executor
    }

    /// Get risk manager reference
    pub fn risk_manager(&self) -> &Arc<RiskManager> {
        &self.risk_manager
    }

    /// Get position manager reference
    pub fn position_manager(&self) -> &Arc<PositionManager> {
        &self.position_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::PolymarketClient;

    // Basic tests would go here
}
