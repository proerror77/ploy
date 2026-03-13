//! Strategy Manager
//!
//! Manages the lifecycle of trading strategies:
//! - Start/stop strategies
//! - Route market data and events
//! - Track running strategies
//! - Provide status information

use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};
use tokio::task::JoinHandle;
use tracing::{error, info};

use super::traits::{MarketUpdate, OrderUpdate, Strategy, StrategyAction};
use crate::error::Result;

mod factory;
mod lifecycle;

pub use factory::{StrategyFactory, StrategyInfo};
use lifecycle::RunningStrategy;
pub use lifecycle::StrategyStatus;

// ============================================================================
// Strategy Manager
// ============================================================================

/// Manages running strategies and routes events to them
pub struct StrategyManager {
    /// Running strategy instances
    strategies: Arc<RwLock<HashMap<String, RunningStrategy>>>,
    /// Channel for broadcasting market updates
    market_tx: broadcast::Sender<MarketUpdate>,
    /// Channel for broadcasting order updates
    order_tx: broadcast::Sender<OrderUpdate>,
    /// Channel for strategy actions (orders, alerts, etc.)
    action_tx: mpsc::Sender<(String, StrategyAction)>,
    /// Action receiver (for executor to consume)
    action_rx: Arc<RwLock<Option<mpsc::Receiver<(String, StrategyAction)>>>>,
    /// Tick interval for periodic strategy updates
    tick_interval_ms: u64,
    /// Shutdown signal
    shutdown_tx: broadcast::Sender<()>,
}

impl StrategyManager {
    /// Create a new strategy manager
    pub fn new(tick_interval_ms: u64) -> Self {
        let (market_tx, _) = broadcast::channel(1024);
        let (order_tx, _) = broadcast::channel(256);
        let (action_tx, action_rx) = mpsc::channel(256);
        let (shutdown_tx, _) = broadcast::channel(16);

        Self {
            strategies: Arc::new(RwLock::new(HashMap::new())),
            market_tx,
            order_tx,
            action_tx,
            action_rx: Arc::new(RwLock::new(Some(action_rx))),
            tick_interval_ms,
            shutdown_tx,
        }
    }

    /// Take the action receiver (can only be called once)
    pub async fn take_action_receiver(&self) -> Option<mpsc::Receiver<(String, StrategyAction)>> {
        self.action_rx.write().await.take()
    }

    /// Broadcast a market update to all strategies
    pub fn send_market_update(&self, update: MarketUpdate) {
        let _ = self.market_tx.send(update);
    }

    /// Broadcast an order update to all strategies
    pub fn send_order_update(&self, update: OrderUpdate) {
        let _ = self.order_tx.send(update);
    }

    /// Spawn the background task for a strategy
    async fn spawn_strategy_task(
        &self,
        strategy_id: String,
        strategy: Arc<RwLock<Box<dyn Strategy>>>,
    ) -> JoinHandle<()> {
        let mut market_rx = self.market_tx.subscribe();
        let mut order_rx = self.order_tx.subscribe();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let action_tx = self.action_tx.clone();
        let tick_interval = tokio::time::Duration::from_millis(self.tick_interval_ms);

        tokio::spawn(async move {
            let mut tick_interval = tokio::time::interval(tick_interval);

            loop {
                tokio::select! {
                    // Handle market updates
                    Ok(update) = market_rx.recv() => {
                        let actions = {
                            let mut strategy = strategy.write().await;
                            match strategy.on_market_update(&update).await {
                                Ok(actions) => actions,
                                Err(e) => {
                                    error!("Strategy {} market update error: {}", strategy_id, e);
                                    continue;
                                }
                            }
                        };

                        let mut dispatch_failed = false;
                        for action in actions {
                            if let Err(err) = action_tx.send((strategy_id.clone(), action)).await {
                                error!(
                                    "Strategy {} failed to dispatch market action (executor channel closed): {}",
                                    strategy_id, err
                                );
                                dispatch_failed = true;
                                break;
                            }
                        }
                        if dispatch_failed {
                            break;
                        }
                    }

                    // Handle order updates
                    Ok(update) = order_rx.recv() => {
                        let actions = {
                            let mut strategy = strategy.write().await;
                            match strategy.on_order_update(&update).await {
                                Ok(actions) => actions,
                                Err(e) => {
                                    error!("Strategy {} order update error: {}", strategy_id, e);
                                    continue;
                                }
                            }
                        };

                        let mut dispatch_failed = false;
                        for action in actions {
                            if let Err(err) = action_tx.send((strategy_id.clone(), action)).await {
                                error!(
                                    "Strategy {} failed to dispatch order action (executor channel closed): {}",
                                    strategy_id, err
                                );
                                dispatch_failed = true;
                                break;
                            }
                        }
                        if dispatch_failed {
                            break;
                        }
                    }

                    // Handle periodic ticks
                    _ = tick_interval.tick() => {
                        let actions = {
                            let mut strategy = strategy.write().await;
                            match strategy.on_tick(Utc::now()).await {
                                Ok(actions) => actions,
                                Err(e) => {
                                    error!("Strategy {} tick error: {}", strategy_id, e);
                                    continue;
                                }
                            }
                        };

                        let mut dispatch_failed = false;
                        for action in actions {
                            if let Err(err) = action_tx.send((strategy_id.clone(), action)).await {
                                error!(
                                    "Strategy {} failed to dispatch tick action (executor channel closed): {}",
                                    strategy_id, err
                                );
                                dispatch_failed = true;
                                break;
                            }
                        }
                        if dispatch_failed {
                            break;
                        }
                    }

                    // Handle shutdown
                    _ = shutdown_rx.recv() => {
                        info!("Strategy {} received shutdown signal", strategy_id);
                        break;
                    }
                }
            }
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::traits::{
        AlertLevel, DataFeed, MarketUpdate, OrderUpdate, Strategy, StrategyAction,
        StrategyStateInfo,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use tokio::time::{timeout, Duration};

    struct TestStrategy {
        id: String,
        name: String,
        shutdown_actions: Vec<StrategyAction>,
    }

    impl TestStrategy {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                name: "test_strategy".to_string(),
                shutdown_actions: Vec::new(),
            }
        }

        fn with_shutdown_action(id: &str) -> Self {
            Self {
                id: id.to_string(),
                name: "test_strategy".to_string(),
                shutdown_actions: vec![StrategyAction::Alert {
                    level: AlertLevel::Warning,
                    message: "shutdown".to_string(),
                }],
            }
        }
    }

    #[async_trait]
    impl Strategy for TestStrategy {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "test"
        }

        fn required_feeds(&self) -> Vec<DataFeed> {
            vec![DataFeed::BinanceSpot {
                symbols: vec!["BTCUSDT".to_string()],
            }]
        }

        async fn on_market_update(
            &mut self,
            _update: &MarketUpdate,
        ) -> crate::error::Result<Vec<StrategyAction>> {
            Ok(vec![StrategyAction::Alert {
                level: AlertLevel::Info,
                message: "market_update".to_string(),
            }])
        }

        async fn on_order_update(
            &mut self,
            _update: &OrderUpdate,
        ) -> crate::error::Result<Vec<StrategyAction>> {
            Ok(vec![StrategyAction::Alert {
                level: AlertLevel::Info,
                message: "order_update".to_string(),
            }])
        }

        async fn on_tick(
            &mut self,
            _now: chrono::DateTime<Utc>,
        ) -> crate::error::Result<Vec<StrategyAction>> {
            Ok(Vec::new())
        }

        fn state(&self) -> StrategyStateInfo {
            StrategyStateInfo {
                strategy_id: self.id.clone(),
                enabled: true,
                ..StrategyStateInfo::default()
            }
        }

        fn positions(&self) -> Vec<crate::strategy::traits::PositionInfo> {
            Vec::new()
        }

        fn is_active(&self) -> bool {
            true
        }

        async fn shutdown(&mut self) -> crate::error::Result<Vec<StrategyAction>> {
            Ok(self.shutdown_actions.clone())
        }

        fn reset(&mut self) {}
    }

    #[tokio::test]
    async fn test_strategy_manager_creation() {
        let manager = StrategyManager::new(1000);
        assert!(manager.list_running().await.is_empty());
    }

    #[tokio::test]
    async fn test_market_update_routed_to_running_strategy() {
        let manager = StrategyManager::new(60_000);
        let mut action_rx = manager
            .take_action_receiver()
            .await
            .expect("action receiver should be available");

        manager
            .start_strategy(Box::new(TestStrategy::new("s1")), None)
            .await
            .expect("start strategy");

        manager.send_market_update(MarketUpdate::BinancePrice {
            symbol: "BTCUSDT".to_string(),
            price: dec!(43210.5),
            timestamp: Utc::now(),
        });

        let (strategy_id, action) = timeout(Duration::from_secs(1), action_rx.recv())
            .await
            .expect("receive timeout")
            .expect("channel closed");
        assert_eq!(strategy_id, "s1");
        match action {
            StrategyAction::Alert { message, .. } => assert_eq!(message, "market_update"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_order_update_routed_to_running_strategy() {
        let manager = StrategyManager::new(60_000);
        let mut action_rx = manager
            .take_action_receiver()
            .await
            .expect("action receiver should be available");

        manager
            .start_strategy(Box::new(TestStrategy::new("s2")), None)
            .await
            .expect("start strategy");

        manager.send_order_update(OrderUpdate {
            order_id: "o1".to_string(),
            client_order_id: Some("c1".to_string()),
            status: crate::domain::OrderStatus::Filled,
            filled_qty: 10,
            avg_fill_price: Some(dec!(0.42)),
            timestamp: Utc::now(),
            error: None,
        });

        let (strategy_id, action) = timeout(Duration::from_secs(1), action_rx.recv())
            .await
            .expect("receive timeout")
            .expect("channel closed");
        assert_eq!(strategy_id, "s2");
        match action {
            StrategyAction::Alert { message, .. } => assert_eq!(message, "order_update"),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_graceful_stop_reports_closed_action_channel() {
        let manager = StrategyManager::new(60_000);
        let action_rx = manager
            .take_action_receiver()
            .await
            .expect("action receiver should be available");
        drop(action_rx);

        manager
            .start_strategy(Box::new(TestStrategy::with_shutdown_action("s-stop")), None)
            .await
            .expect("start strategy");

        let err = manager
            .stop_strategy("s-stop", true)
            .await
            .expect_err("closed action channel should be surfaced");
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to send shutdown action"),
            "unexpected error message: {}",
            msg
        );
    }
}
