//! Data Feed Manager
//!
//! Coordinates data feeds from Binance and Polymarket, converting their
//! updates to MarketUpdate events for the StrategyManager.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::manager::StrategyManager;
use super::traits::DataFeed;
use crate::adapters::{
    BinanceKlineWebSocket, BinanceWebSocket, PolymarketClient, PolymarketWebSocket,
};

mod polymarket_events;
mod runtime;

use polymarket_events::{DiscoveredEvent, EventMapping};

const MAX_EVENTS_PER_SERIES: usize = 6;
const POLYMARKET_REFRESH_SECS: u64 = 30;

fn l2_feed_enabled() -> bool {
    std::env::var("PLOY_BINANCE_L2_FEED_ENABLED")
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "y" | "on"
            )
        })
        .unwrap_or(true)
}

/// Manages data feeds and routes updates to StrategyManager
pub struct DataFeedManager {
    /// Reference to strategy manager
    manager: Arc<StrategyManager>,
    /// Optional shared data plane owner for WS lifecycles.
    data_plane: Option<Arc<crate::data_plane::PlatformDataPlane>>,
    /// Binance WebSocket (optional)
    binance_ws: Option<Arc<BinanceWebSocket>>,
    /// Binance Kline WebSocket (optional)
    binance_kline_ws: Option<Arc<BinanceKlineWebSocket>>,
    /// Binance kline symbols (for optional REST backfill + dedupe)
    binance_kline_symbols: Vec<String>,
    /// Binance kline intervals (for optional REST backfill + dedupe)
    binance_kline_intervals: Vec<String>,
    /// Backfill limit per (symbol, interval). `0` disables backfill.
    binance_kline_backfill_limit: usize,
    /// Last seen `close_time` per (symbol, interval) to skip duplicates.
    binance_kline_last_close: Arc<RwLock<HashMap<String, HashMap<String, DateTime<Utc>>>>>,
    /// Polymarket WebSocket (optional)
    polymarket_ws: Option<Arc<PolymarketWebSocket>>,
    /// Polymarket client for event discovery
    pm_client: Option<Arc<PolymarketClient>>,
    /// Token to event mapping for Polymarket
    #[allow(dead_code)]
    token_events: Arc<RwLock<HashMap<String, EventMapping>>>,
    /// Active feeds
    #[allow(dead_code)]
    active_feeds: Arc<RwLock<Vec<DataFeed>>>,
    /// Latest discovered events per series (bounded, for refresh + token reconciliation)
    series_events: Arc<RwLock<HashMap<String, HashMap<String, DiscoveredEvent>>>>,
    /// Optional DB pool used to persist normalized market metadata for model training.
    metadata_pool: Option<Arc<PgPool>>,
    /// Guard to avoid starting Binance L2 feed more than once per manager.
    binance_l2_started: Arc<RwLock<bool>>,
}

impl DataFeedManager {
    /// Create a new DataFeedManager
    pub fn new(manager: Arc<StrategyManager>) -> Self {
        let metadata_pool = std::env::var("PLOY_DATABASE__URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .and_then(|url| {
                PgPoolOptions::new()
                    .max_connections(2)
                    .connect_lazy(&url)
                    .ok()
            })
            .map(Arc::new);
        Self {
            manager,
            data_plane: None,
            binance_ws: None,
            binance_kline_ws: None,
            binance_kline_symbols: Vec::new(),
            binance_kline_intervals: Vec::new(),
            binance_kline_backfill_limit: 0,
            binance_kline_last_close: Arc::new(RwLock::new(HashMap::new())),
            polymarket_ws: None,
            pm_client: None,
            token_events: Arc::new(RwLock::new(HashMap::new())),
            active_feeds: Arc::new(RwLock::new(Vec::new())),
            series_events: Arc::new(RwLock::new(HashMap::new())),
            metadata_pool,
            binance_l2_started: Arc::new(RwLock::new(false)),
        }
    }

    /// Create a DataFeedManager backed by a shared PlatformDataPlane.
    pub fn from_data_plane(
        dp: Arc<crate::data_plane::PlatformDataPlane>,
        manager: Arc<StrategyManager>,
    ) -> Self {
        let metadata_pool = std::env::var("PLOY_DATABASE__URL")
            .ok()
            .or_else(|| std::env::var("DATABASE_URL").ok())
            .and_then(|url| {
                PgPoolOptions::new()
                    .max_connections(2)
                    .connect_lazy(&url)
                    .ok()
            })
            .map(Arc::new);

        let (binance_kline_symbols, binance_kline_intervals) = {
            let cfg = dp.config();
            (
                cfg.binance_kline_symbols.clone(),
                cfg.binance_kline_intervals.clone(),
            )
        };

        Self {
            manager,
            data_plane: Some(dp.clone()),
            binance_ws: dp.binance_ws(),
            binance_kline_ws: dp.binance_kline_ws(),
            binance_kline_symbols,
            binance_kline_intervals,
            binance_kline_backfill_limit: std::env::var("PLOY_BINANCE_KLINE_BACKFILL_LIMIT")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(300),
            binance_kline_last_close: Arc::new(RwLock::new(HashMap::new())),
            polymarket_ws: dp.polymarket_ws(),
            pm_client: None,
            token_events: Arc::new(RwLock::new(HashMap::new())),
            active_feeds: Arc::new(RwLock::new(Vec::new())),
            series_events: Arc::new(RwLock::new(HashMap::new())),
            metadata_pool,
            binance_l2_started: Arc::new(RwLock::new(false)),
        }
    }

    /// Configure Binance feed for given symbols
    pub fn with_binance(mut self, symbols: Vec<String>) -> Self {
        if !symbols.is_empty() {
            self.binance_ws = Some(Arc::new(BinanceWebSocket::new(symbols)));
        }
        self
    }

    /// Configure Binance kline feed for given symbols/intervals.
    pub fn with_binance_klines(
        mut self,
        symbols: Vec<String>,
        intervals: Vec<String>,
        closed_only: bool,
        backfill_limit: usize,
    ) -> Self {
        if !symbols.is_empty() && !intervals.is_empty() {
            self.binance_kline_symbols = symbols.clone();
            self.binance_kline_intervals = intervals.clone();
            self.binance_kline_backfill_limit = backfill_limit;
            self.binance_kline_ws = Some(Arc::new(BinanceKlineWebSocket::new(
                symbols,
                intervals,
                closed_only,
            )));
        }
        self
    }

    /// Configure Polymarket feed
    pub fn with_polymarket(mut self, ws: PolymarketWebSocket, client: PolymarketClient) -> Self {
        self.polymarket_ws = Some(Arc::new(ws));
        self.pm_client = Some(Arc::new(client));
        self
    }

    /// Inject a Polymarket REST client without creating another WS instance.
    pub fn with_pm_client(mut self, client: PolymarketClient) -> Self {
        self.pm_client = Some(Arc::new(client));
        self
    }
}

/// Builder for creating a DataFeedManager with strategy requirements
pub struct DataFeedBuilder {
    symbols: Vec<String>,
    series_ids: Vec<String>,
}

impl DataFeedBuilder {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            series_ids: Vec::new(),
        }
    }

    pub fn with_symbols(mut self, symbols: Vec<String>) -> Self {
        self.symbols.extend(symbols);
        self
    }

    pub fn with_series(mut self, series_ids: Vec<String>) -> Self {
        self.series_ids.extend(series_ids);
        self
    }

    pub fn build_binance(&self) -> Option<BinanceWebSocket> {
        if self.symbols.is_empty() {
            None
        } else {
            Some(BinanceWebSocket::new(self.symbols.clone()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_plane::{DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane};
    use crate::strategy::traits::{
        AlertLevel, DataFeed, MarketUpdate, OrderUpdate, Strategy, StrategyAction,
        StrategyStateInfo,
    };
    use async_trait::async_trait;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    struct FeedCaptureStrategy {
        id: String,
    }

    impl FeedCaptureStrategy {
        fn new(id: &str) -> Self {
            Self { id: id.to_string() }
        }
    }

    #[async_trait]
    impl Strategy for FeedCaptureStrategy {
        fn id(&self) -> &str {
            &self.id
        }

        fn name(&self) -> &str {
            "feed_capture"
        }

        fn description(&self) -> &str {
            "capture market update kinds for tests"
        }

        fn required_feeds(&self) -> Vec<DataFeed> {
            vec![]
        }

        async fn on_market_update(
            &mut self,
            update: &MarketUpdate,
        ) -> crate::error::Result<Vec<StrategyAction>> {
            let tag = match update {
                MarketUpdate::PolymarketQuote { .. } => "polymarket_quote",
                MarketUpdate::BinancePrice { .. } => "binance_price",
                MarketUpdate::BinanceL2 { .. } => "binance_l2",
                MarketUpdate::BinanceKline { .. } => "binance_kline",
                MarketUpdate::EventDiscovered { .. } => "event_discovered",
                MarketUpdate::EventExpired { .. } => "event_expired",
                MarketUpdate::BinanceFunding { .. } => "binance_funding",
                MarketUpdate::BinanceLiquidation { .. } => "binance_liquidation",
                MarketUpdate::DeribitIV { .. } => "deribit_iv",
            };
            Ok(vec![StrategyAction::Alert {
                level: AlertLevel::Info,
                message: format!("market:{}", tag),
            }])
        }

        async fn on_order_update(
            &mut self,
            _update: &OrderUpdate,
        ) -> crate::error::Result<Vec<StrategyAction>> {
            Ok(Vec::new())
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
            Ok(Vec::new())
        }

        fn reset(&mut self) {}
    }

    async fn setup_manager_with_strategy(
        strategy_id: &str,
    ) -> (
        Arc<StrategyManager>,
        mpsc::Receiver<(String, StrategyAction)>,
    ) {
        let manager = Arc::new(StrategyManager::new(60_000));
        let action_rx = manager
            .take_action_receiver()
            .await
            .expect("action receiver should be available");
        manager
            .start_strategy(Box::new(FeedCaptureStrategy::new(strategy_id)), None)
            .await
            .expect("start strategy");
        (manager, action_rx)
    }

    async fn recv_market_alert(
        action_rx: &mut mpsc::Receiver<(String, StrategyAction)>,
    ) -> (String, String) {
        let (strategy_id, action) = timeout(Duration::from_secs(1), action_rx.recv())
            .await
            .expect("receive timeout")
            .expect("action channel closed");
        match action {
            StrategyAction::Alert { message, .. } => (strategy_id, message),
            other => panic!("unexpected action: {:?}", other),
        }
    }

    #[test]
    fn test_feed_builder() {
        let builder = DataFeedBuilder::new()
            .with_symbols(vec!["BTCUSDT".into(), "ETHUSDT".into()])
            .with_series(vec!["10192".into()]);

        let binance = builder.build_binance();
        assert!(binance.is_some());
    }

    #[test]
    fn test_feed_builder_empty_symbols_returns_none() {
        let builder = DataFeedBuilder::new();
        assert!(builder.build_binance().is_none());
    }

    #[tokio::test]
    async fn test_from_data_plane_reuses_singleton_adapters() {
        let manager = Arc::new(StrategyManager::new(1000));
        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                polymarket_ws_url: "wss://example.invalid/ws".to_string(),
                binance_spot_symbols: vec!["BTCUSDT".to_string()],
                ..DataPlaneConfig::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));

        let feed = DataFeedManager::from_data_plane(data_plane.clone(), manager);
        assert!(feed.data_plane.is_some());
        assert!(feed.pm_client.is_none());

        let feed_bn = feed.binance_ws.as_ref().expect("feed binance ws");
        let dp_bn = data_plane.binance_ws().expect("dp binance ws");
        assert!(Arc::ptr_eq(feed_bn, &dp_bn));

        let feed_pm = feed.polymarket_ws.as_ref().expect("feed pm ws");
        let dp_pm = data_plane.polymarket_ws().expect("dp pm ws");
        assert!(Arc::ptr_eq(feed_pm, &dp_pm));
    }

    #[tokio::test]
    async fn characterization_replay_binance_price_to_strategy_market_update() {
        let (manager, mut action_rx) = setup_manager_with_strategy("feed_s1").await;
        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                binance_spot_symbols: vec!["BTCUSDT".to_string()],
                ..DataPlaneConfig::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));

        let feed = DataFeedManager::from_data_plane(data_plane.clone(), manager);
        feed.start().await.expect("start feed manager");
        tokio::time::sleep(Duration::from_millis(25)).await;

        let ws = data_plane.binance_ws().expect("binance ws");
        ws.ingest_test_message(
            r#"{"e":"aggTrade","E":1700000000000,"s":"BTCUSDT","p":"43250.50","q":"0.123","T":1700000000000}"#,
        )
        .await;

        let (sid, message) = recv_market_alert(&mut action_rx).await;
        assert_eq!(sid, "feed_s1");
        assert_eq!(message, "market:binance_price");
    }

    #[tokio::test]
    async fn characterization_replay_polymarket_quote_to_strategy_market_update() {
        let (manager, mut action_rx) = setup_manager_with_strategy("feed_s2").await;
        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                polymarket_ws_url: "wss://example.invalid/ws".to_string(),
                ..DataPlaneConfig::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));

        let pm_ws = data_plane.polymarket_ws().expect("pm ws");
        pm_ws
            .register_token("0xabc123", crate::domain::Side::Up)
            .await;

        let feed = DataFeedManager::from_data_plane(data_plane.clone(), manager);
        feed.start().await.expect("start feed manager");
        tokio::time::sleep(Duration::from_millis(25)).await;

        let handled = pm_ws.ingest_test_message(
            r#"[{"asset_id":"0xabc123","market":"0xmarket","bids":[{"price":"0.45","size":"100"}],"asks":[{"price":"0.47","size":"50"}]}]"#,
        ).await;
        assert!(handled, "polymarket message should be handled");

        let (sid, message) = recv_market_alert(&mut action_rx).await;
        assert_eq!(sid, "feed_s2");
        assert_eq!(message, "market:polymarket_quote");
    }

    #[tokio::test]
    async fn characterization_replay_binance_kline_to_strategy_market_update() {
        let (manager, mut action_rx) = setup_manager_with_strategy("feed_s3").await;
        let data_plane = Arc::new(PlatformDataPlane::new(
            DataPlaneConfig {
                binance_kline_symbols: vec!["BTCUSDT".to_string()],
                binance_kline_intervals: vec!["5m".to_string()],
                binance_kline_closed_only: true,
                ..DataPlaneConfig::default()
            },
            Arc::new(DataPlaneFreshness::new()),
        ));

        let mut feed = DataFeedManager::from_data_plane(data_plane.clone(), manager);
        // Keep test deterministic/offline: skip REST backfill.
        feed.binance_kline_backfill_limit = 0;
        feed.start().await.expect("start feed manager");
        tokio::time::sleep(Duration::from_millis(25)).await;

        let ws = data_plane.binance_kline_ws().expect("binance kline ws");
        ws.ingest_test_message(
            r#"{
                "stream":"btcusdt@kline_5m",
                "data":{
                    "e":"kline",
                    "E":1700000000000,
                    "s":"BTCUSDT",
                    "k":{
                        "t":1700000000000,
                        "T":1700000299999,
                        "s":"BTCUSDT",
                        "i":"5m",
                        "f":0,
                        "L":0,
                        "o":"100.0",
                        "c":"101.0",
                        "h":"102.0",
                        "l":"99.0",
                        "v":"123.4",
                        "n":0,
                        "x":true,
                        "q":"0",
                        "V":"0",
                        "Q":"0",
                        "B":"0"
                    }
                }
            }"#,
        )
        .await;

        let (sid, message) = recv_market_alert(&mut action_rx).await;
        assert_eq!(sid, "feed_s3");
        assert_eq!(message, "market:binance_kline");
    }
}
