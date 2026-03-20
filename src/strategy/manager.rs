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
use anyhow::anyhow;

mod lifecycle;

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
// Strategy Factory
// ============================================================================

/// Factory for creating strategy instances from configuration
pub struct StrategyFactory;

impl StrategyFactory {
    /// Create a strategy from a TOML configuration string
    pub fn from_toml(config_content: &str, dry_run: bool) -> Result<Box<dyn Strategy>> {
        use toml::Value;

        let config: Value =
            toml::from_str(config_content).map_err(|e| anyhow!("Invalid TOML: {}", e))?;

        let strategy_section = config
            .get("strategy")
            .ok_or_else(|| anyhow!("Missing [strategy] section"))?;

        let strategy_name = strategy_section
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing strategy.name"))?;

        let strategy_id = format!("{}_{}", strategy_name, chrono::Utc::now().timestamp());

        match strategy_name {
            "momentum" => {
                let adapter = super::adapters::MomentumStrategyAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "split_arb" => {
                let adapter = super::adapters::SplitArbStrategyAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "pattern_memory" => {
                let strat = super::pattern_memory::PatternMemoryStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strat))
            }
            "staggered_arb" | "gamma_scalping" => {
                let adapter = super::staggered_arb_live::StaggeredArbAdapter::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(adapter))
            }
            "event_edge" => {
                let strategy = super::event_edge::strategy::EventEdgeStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strategy))
            }
            "crypto_lob_ml" => {
                let strategy = super::crypto_lob_ml::strategy::CryptoLobMlStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strategy))
            }
            "crypto_rl_policy" => {
                let strategy =
                    super::crypto_rl_policy::strategy::CryptoRlPolicyStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            "pm_5m_directional" => {
                let strategy = super::pm_5m_directional::Pm5mDirectionalStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strategy))
            }
            "nba_comeback" => {
                let strategy = super::nba_comeback::strategy::NbaComebackStrategy::from_toml(
                    strategy_id,
                    config_content,
                    dry_run,
                )?;
                Ok(Box::new(strategy))
            }
            "weather_market" => {
                let strategy =
                    super::weather_market::strategy::WeatherMarketStrategy::from_toml(
                        strategy_id,
                        config_content,
                        dry_run,
                    )?;
                Ok(Box::new(strategy))
            }
            other => Err(anyhow!("Unknown strategy type: {}", other).into()),
        }
    }

    /// Get list of available strategy types
    pub fn available_strategies() -> Vec<StrategyInfo> {
        vec![
            StrategyInfo {
                name: "momentum".to_string(),
                description: "Trade crypto UP/DOWN based on CEX price momentum".to_string(),
                config_template: "momentum_default.toml".to_string(),
            },
            StrategyInfo {
                name: "split_arb".to_string(),
                description: "Split arbitrage when YES+NO prices < $1".to_string(),
                config_template: "split_arb_default.toml".to_string(),
            },
            StrategyInfo {
                name: "pattern_memory".to_string(),
                description: "Associative memory on Binance klines for 5m UP/DOWN".to_string(),
                config_template: "pattern_memory_default.toml".to_string(),
            },
            StrategyInfo {
                name: "staggered_arb".to_string(),
                description: "Time-staggered two-leg arb on crypto UP/DOWN binary options; aliases: gamma_scalping, staggered-arb"
                    .to_string(),
                config_template: "staggered_arb.toml".to_string(),
            },
            StrategyInfo {
                name: "event_edge".to_string(),
                description: "Politics/event-driven edge strategy on Polymarket events"
                    .to_string(),
                config_template: "event_edge.toml".to_string(),
            },
            StrategyInfo {
                name: "crypto_lob_ml".to_string(),
                description:
                    "Canonical observe-only crypto LOB ML wrapper for runtime migration"
                        .to_string(),
                config_template: "crypto_lob_ml.toml".to_string(),
            },
            StrategyInfo {
                name: "crypto_rl_policy".to_string(),
                description:
                    "Canonical observe-only crypto RL policy wrapper for runtime migration"
                        .to_string(),
                config_template: "crypto_rl_policy.toml".to_string(),
            },
            StrategyInfo {
                name: "pm_5m_directional".to_string(),
                description:
                    "Standalone Polymarket 5m directional strategy driven by Binance direction and PM cost gates"
                        .to_string(),
                config_template: "pm_5m_directional_default.toml".to_string(),
            },
            StrategyInfo {
                name: "nba_comeback".to_string(),
                description: "NBA comeback strategy using ESPN/game-state inputs".to_string(),
                config_template: "nba_comeback.toml".to_string(),
            },
            StrategyInfo {
                name: "weather_market".to_string(),
                description:
                    "Observe-only weather contract strategy using public station and forecast data"
                        .to_string(),
                config_template: "weather_market_default.toml".to_string(),
            },
        ]
    }
}

/// Information about an available strategy type
#[derive(Debug, Clone)]
pub struct StrategyInfo {
    /// Strategy name/type
    pub name: String,
    /// Description
    pub description: String,
    /// Default config template filename
    pub config_template: String,
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

    #[test]
    fn test_available_strategies() {
        let strategies = StrategyFactory::available_strategies();
        assert!(!strategies.is_empty());
        assert!(strategies.iter().any(|s| s.name == "momentum"));
        assert!(strategies.iter().any(|s| s.name == "event_edge"));
        assert!(strategies.iter().any(|s| s.name == "nba_comeback"));
        assert!(strategies.iter().any(|s| s.name == "weather_market"));
    }

    #[test]
    fn test_available_strategies_include_pm_5m_directional() {
        let strategies = StrategyFactory::available_strategies();
        assert!(strategies.iter().any(|s| s.name == "pm_5m_directional"));
        assert!(strategies.iter().any(|s| {
            s.name == "pm_5m_directional" && s.config_template == "pm_5m_directional_default.toml"
        }));
    }

    #[test]
    fn test_factory_builds_pm_5m_directional_from_toml() {
        let config = r#"
[strategy]
name = "pm_5m_directional"
enabled = true

[pm_5m_directional]
symbols = ["BTCUSDT"]
"#;

        let strategy = StrategyFactory::from_toml(config, true).expect("strategy");
        assert_eq!(strategy.name(), "pm_5m_directional");
        assert!(strategy.required_feeds().iter().any(|feed| matches!(
            feed,
            DataFeed::PolymarketEvents { series_ids } if series_ids == &vec!["10684".to_string()]
        )));
    }

    #[test]
    fn test_factory_builds_weather_market_from_toml() {
        let config = r#"
[strategy]
name = "weather_market"
enabled = true

[weather_market]
station_id = "KJFK"
station_name = "JFK"
contract_date = "2026-03-20"
latitude = 40.64
longitude = -73.78
station_utc_offset_hours = -4

[[weather_market.buckets]]
label = "70-72F"
token_id = "token-a"
min_temp = 70.0
max_temp = 73.0
"#;

        let strategy = StrategyFactory::from_toml(config, true).expect("strategy");
        assert_eq!(strategy.name(), "weather_market");
        assert!(strategy.required_feeds().iter().any(|feed| matches!(
            feed,
            DataFeed::Tick { interval_ms } if *interval_ms == 300_000
        )));
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
