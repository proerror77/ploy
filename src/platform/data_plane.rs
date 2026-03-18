//! Platform data plane orchestration for shared market-data adapters.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::sync::broadcast;
use tracing::error;

use super::freshness::{DataPlaneFreshness, DataSource};
use crate::adapters::polymarket_ws::BookMessage;
use crate::adapters::{
    BinanceKlineWebSocket, BinanceWebSocket, ChainlinkRtds, ChainlinkUpdate, KlineUpdate,
    PolymarketWebSocket, PriceUpdate, QuoteUpdate,
};
use crate::error::{PloyError, Result};

/// Runtime configuration for the platform data plane.
#[derive(Debug, Clone, Default)]
pub struct DataPlaneConfig {
    pub polymarket_ws_url: String,
    pub binance_spot_symbols: Vec<String>,
    pub binance_kline_symbols: Vec<String>,
    pub binance_kline_intervals: Vec<String>,
    pub binance_kline_closed_only: bool,
    pub chainlink_symbols: Vec<String>,
}

impl DataPlaneConfig {
    fn validate(&self, pm_required: bool) -> Result<()> {
        if pm_required && self.polymarket_ws_url.trim().is_empty() {
            return Err(PloyError::Validation(
                "polymarket_ws_url is required when Polymarket is configured".to_string(),
            ));
        }

        let has_kline_symbols = !self.binance_kline_symbols.is_empty();
        let has_kline_intervals = !self.binance_kline_intervals.is_empty();
        if has_kline_symbols != has_kline_intervals {
            return Err(PloyError::Validation(
                "binance_kline_symbols and binance_kline_intervals must both be set or both be empty".to_string(),
            ));
        }

        Ok(())
    }
}

/// Health status for a configured data source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceHealth {
    Healthy,
    Degraded,
    Down,
}

/// Health snapshot across all optional PlatformDataPlane sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataPlaneHealth {
    pub binance_spot: Option<SourceHealth>,
    pub binance_kline: Option<SourceHealth>,
    pub polymarket_ws: Option<SourceHealth>,
    pub chainlink_rtds: Option<SourceHealth>,
}

/// Reusable handle for crypto market data adapters.
#[derive(Clone)]
pub struct CryptoDataPlaneHandle {
    binance_ws: Arc<BinanceWebSocket>,
    polymarket_ws: Arc<PolymarketWebSocket>,
}

impl CryptoDataPlaneHandle {
    pub fn new(binance_ws: Arc<BinanceWebSocket>, polymarket_ws: Arc<PolymarketWebSocket>) -> Self {
        Self {
            binance_ws,
            polymarket_ws,
        }
    }

    pub fn subscribe_prices(&self) -> broadcast::Receiver<PriceUpdate> {
        self.binance_ws.subscribe()
    }

    pub fn subscribe_quotes(&self) -> broadcast::Receiver<QuoteUpdate> {
        self.polymarket_ws.subscribe_updates()
    }

    pub fn price_cache(&self) -> crate::adapters::PriceCache {
        self.binance_ws.price_cache().clone()
    }

    pub fn quote_cache(&self) -> crate::adapters::polymarket_ws::QuoteCache {
        self.polymarket_ws.quote_cache().clone()
    }

    pub async fn register_tokens(&self, up_token_id: &str, down_token_id: &str) {
        self.polymarket_ws
            .register_tokens(up_token_id, down_token_id)
            .await;
    }

    pub fn request_resubscribe(&self) {
        self.polymarket_ws.request_resubscribe();
    }
}

/// Reusable handle for Binance market-data adapter access.
#[derive(Clone)]
pub struct BinanceDataPlaneHandle {
    binance_ws: Arc<BinanceWebSocket>,
}

impl BinanceDataPlaneHandle {
    pub fn new(binance_ws: Arc<BinanceWebSocket>) -> Self {
        Self { binance_ws }
    }

    pub fn subscribe_prices(&self) -> broadcast::Receiver<PriceUpdate> {
        self.binance_ws.subscribe()
    }

    pub fn price_cache(&self) -> crate::adapters::PriceCache {
        self.binance_ws.price_cache().clone()
    }
}

/// Shared data-plane runtime with optional singleton WS adapters.
pub struct PlatformDataPlane {
    binance_ws: Option<Arc<BinanceWebSocket>>,
    binance_kline_ws: Option<Arc<BinanceKlineWebSocket>>,
    polymarket_ws: Option<Arc<PolymarketWebSocket>>,
    chainlink_ws: Option<Arc<ChainlinkRtds>>,
    freshness: Arc<DataPlaneFreshness>,
    config: DataPlaneConfig,
    started: AtomicBool,
}

impl PlatformDataPlane {
    pub fn new(config: DataPlaneConfig, freshness: Arc<DataPlaneFreshness>) -> Self {
        let binance_ws = if config.binance_spot_symbols.is_empty() {
            None
        } else {
            let ws = Arc::new(BinanceWebSocket::new(config.binance_spot_symbols.clone()));
            ws.set_freshness(
                Arc::clone(&freshness) as Arc<dyn ploy_data::freshness::FreshnessTracker>
            );
            Some(ws)
        };

        let binance_kline_ws = if config.binance_kline_symbols.is_empty()
            || config.binance_kline_intervals.is_empty()
        {
            None
        } else {
            let ws = Arc::new(BinanceKlineWebSocket::new(
                config.binance_kline_symbols.clone(),
                config.binance_kline_intervals.clone(),
                config.binance_kline_closed_only,
            ));
            ws.set_freshness(
                Arc::clone(&freshness) as Arc<dyn ploy_data::freshness::FreshnessTracker>
            );
            Some(ws)
        };

        let polymarket_ws = if config.polymarket_ws_url.trim().is_empty() {
            None
        } else {
            let ws = Arc::new(PolymarketWebSocket::new(config.polymarket_ws_url.trim()));
            ws.set_freshness(Arc::clone(&freshness));
            Some(ws)
        };

        let chainlink_ws = if config.chainlink_symbols.is_empty() {
            None
        } else {
            let ws = Arc::new(ChainlinkRtds::new(config.chainlink_symbols.clone()));
            ws.set_freshness(Arc::clone(&freshness));
            Some(ws)
        };

        Self {
            binance_ws,
            binance_kline_ws,
            polymarket_ws,
            chainlink_ws,
            freshness,
            config,
            started: AtomicBool::new(false),
        }
    }

    pub async fn start(&self, initial_pm_tokens: Vec<String>) -> Result<()> {
        self.config.validate(self.polymarket_ws.is_some())?;

        if self
            .started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(PloyError::Validation(
                "PlatformDataPlane::start called more than once".to_string(),
            ));
        }

        if let Some(ws) = &self.binance_ws {
            let ws = Arc::clone(ws);
            tokio::spawn(async move {
                if let Err(err) = ws.run().await {
                    error!("binance websocket task exited: {}", err);
                }
            });
        }

        if let Some(ws) = &self.binance_kline_ws {
            let ws = Arc::clone(ws);
            tokio::spawn(async move {
                if let Err(err) = ws.run().await {
                    error!("binance kline websocket task exited: {}", err);
                }
            });
        }

        if let Some(ws) = &self.polymarket_ws {
            let ws = Arc::clone(ws);
            tokio::spawn(async move {
                if let Err(err) = ws.run(initial_pm_tokens).await {
                    error!("polymarket websocket task exited: {}", err);
                }
            });
        }

        if let Some(ws) = &self.chainlink_ws {
            let ws = Arc::clone(ws);
            tokio::spawn(async move {
                if let Err(err) = ws.run().await {
                    error!("chainlink websocket task exited: {}", err);
                }
            });
        }

        Ok(())
    }

    pub fn subscribe_quotes(&self) -> Option<broadcast::Receiver<QuoteUpdate>> {
        self.polymarket_ws.as_ref().map(|ws| ws.subscribe_updates())
    }

    pub fn subscribe_prices(&self) -> Option<broadcast::Receiver<PriceUpdate>> {
        self.binance_ws.as_ref().map(|ws| ws.subscribe())
    }

    pub fn subscribe_klines(&self) -> Option<broadcast::Receiver<KlineUpdate>> {
        self.binance_kline_ws.as_ref().map(|ws| ws.subscribe())
    }

    pub fn subscribe_books(&self) -> Option<broadcast::Receiver<Arc<BookMessage>>> {
        self.polymarket_ws.as_ref().map(|ws| ws.subscribe_books())
    }

    pub fn subscribe_chainlink(&self) -> Option<broadcast::Receiver<ChainlinkUpdate>> {
        self.chainlink_ws.as_ref().map(|ws| ws.subscribe())
    }

    pub fn binance_ws(&self) -> Option<Arc<BinanceWebSocket>> {
        self.binance_ws.clone()
    }

    pub fn polymarket_ws(&self) -> Option<Arc<PolymarketWebSocket>> {
        self.polymarket_ws.clone()
    }

    pub fn binance_kline_ws(&self) -> Option<Arc<BinanceKlineWebSocket>> {
        self.binance_kline_ws.clone()
    }

    pub fn chainlink_ws(&self) -> Option<Arc<ChainlinkRtds>> {
        self.chainlink_ws.clone()
    }

    pub fn freshness(&self) -> Arc<DataPlaneFreshness> {
        Arc::clone(&self.freshness)
    }

    /// Compute per-source health using freshness message counts and staleness.
    ///
    /// Rules:
    /// - `Down`: no messages observed yet for the source.
    /// - `Degraded`: messages observed, but at least one tracked symbol is stale.
    /// - `Healthy`: messages observed and no stale symbols.
    pub fn source_health(&self, stale_threshold_secs: f64) -> DataPlaneHealth {
        DataPlaneHealth {
            binance_spot: self.binance_ws.as_ref().map(|_| {
                self.evaluate_source_health(DataSource::BinanceSpot, stale_threshold_secs)
            }),
            binance_kline: self.binance_kline_ws.as_ref().map(|_| {
                self.evaluate_source_health(DataSource::BinanceKline, stale_threshold_secs)
            }),
            polymarket_ws: self.polymarket_ws.as_ref().map(|_| {
                self.evaluate_source_health(DataSource::PolymarketWs, stale_threshold_secs)
            }),
            chainlink_rtds: self.chainlink_ws.as_ref().map(|_| {
                self.evaluate_source_health(DataSource::ChainlinkRtds, stale_threshold_secs)
            }),
        }
    }

    fn evaluate_source_health(
        &self,
        source: DataSource,
        stale_threshold_secs: f64,
    ) -> SourceHealth {
        if self.freshness.source_message_count(source) == 0 {
            return SourceHealth::Down;
        }
        if self
            .freshness
            .stale_symbol_count_for_source(source, stale_threshold_secs)
            > 0
        {
            return SourceHealth::Degraded;
        }
        SourceHealth::Healthy
    }

    pub fn config(&self) -> &DataPlaneConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_requires_polymarket_url_when_pm_is_configured() {
        let config = DataPlaneConfig::default();
        let err = config
            .validate(true)
            .expect_err("expected PM validation error");

        match err {
            PloyError::Validation(msg) => {
                assert!(msg.contains("polymarket_ws_url"));
            }
            other => panic!("expected validation error, got {}", other),
        }
    }

    #[test]
    fn validate_kline_symbol_interval_relationship() {
        let symbols_only = DataPlaneConfig {
            binance_kline_symbols: vec!["BTCUSDT".to_string()],
            ..DataPlaneConfig::default()
        };
        assert!(symbols_only.validate(false).is_err());

        let intervals_only = DataPlaneConfig {
            binance_kline_intervals: vec!["5m".to_string()],
            ..DataPlaneConfig::default()
        };
        assert!(intervals_only.validate(false).is_err());

        let valid = DataPlaneConfig {
            binance_kline_symbols: vec!["BTCUSDT".to_string()],
            binance_kline_intervals: vec!["5m".to_string()],
            ..DataPlaneConfig::default()
        };
        assert!(valid.validate(false).is_ok());
    }

    #[test]
    fn subscription_handles_match_available_adapters() {
        let freshness = Arc::new(DataPlaneFreshness::new());
        let no_pm = PlatformDataPlane::new(
            DataPlaneConfig {
                binance_spot_symbols: vec!["BTCUSDT".to_string()],
                binance_kline_symbols: vec!["BTCUSDT".to_string()],
                binance_kline_intervals: vec!["5m".to_string()],
                chainlink_symbols: vec!["btc/usd".to_string()],
                ..DataPlaneConfig::default()
            },
            Arc::clone(&freshness),
        );

        assert!(no_pm.subscribe_prices().is_some());
        assert!(no_pm.subscribe_klines().is_some());
        assert!(no_pm.subscribe_chainlink().is_some());
        assert!(no_pm.subscribe_quotes().is_none());
        assert!(no_pm.subscribe_books().is_none());

        let pm_only = PlatformDataPlane::new(
            DataPlaneConfig {
                polymarket_ws_url: "wss://example.invalid/ws".to_string(),
                ..DataPlaneConfig::default()
            },
            freshness,
        );

        assert!(pm_only.subscribe_quotes().is_some());
        assert!(pm_only.subscribe_books().is_some());
        assert!(pm_only.subscribe_prices().is_none());
        assert!(pm_only.subscribe_klines().is_none());
        assert!(pm_only.subscribe_chainlink().is_none());
    }

    #[tokio::test]
    async fn source_health_reports_down_healthy_and_degraded() {
        let freshness = Arc::new(DataPlaneFreshness::new());
        let plane = PlatformDataPlane::new(
            DataPlaneConfig {
                polymarket_ws_url: "wss://example.invalid/ws".to_string(),
                binance_spot_symbols: vec!["BTCUSDT".to_string()],
                ..DataPlaneConfig::default()
            },
            Arc::clone(&freshness),
        );

        // No messages observed yet.
        let down = plane.source_health(60.0);
        assert_eq!(down.binance_spot, Some(SourceHealth::Down));
        assert_eq!(down.polymarket_ws, Some(SourceHealth::Down));

        // Fresh updates -> healthy.
        freshness.record_update(DataSource::BinanceSpot, "BTCUSDT");
        freshness.record_update(DataSource::PolymarketWs, "tok-1");
        let healthy = plane.source_health(60.0);
        assert_eq!(healthy.binance_spot, Some(SourceHealth::Healthy));
        assert_eq!(healthy.polymarket_ws, Some(SourceHealth::Healthy));

        // Tight threshold after small delay -> degraded.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        let degraded = plane.source_health(0.001);
        assert_eq!(degraded.binance_spot, Some(SourceHealth::Degraded));
        assert_eq!(degraded.polymarket_ws, Some(SourceHealth::Degraded));
    }

    #[tokio::test]
    async fn start_allows_initial_pm_tokens_when_pm_not_configured() {
        let plane = PlatformDataPlane::new(
            DataPlaneConfig::default(),
            Arc::new(DataPlaneFreshness::new()),
        );
        assert!(plane.start(vec!["token-1".to_string()]).await.is_ok());
    }

    #[tokio::test]
    async fn start_second_call_returns_error() {
        let plane = PlatformDataPlane::new(
            DataPlaneConfig::default(),
            Arc::new(DataPlaneFreshness::new()),
        );
        plane
            .start(Vec::new())
            .await
            .expect("first start should work");

        let err = plane
            .start(Vec::new())
            .await
            .expect_err("second start should fail");
        match err {
            PloyError::Validation(msg) => {
                assert!(
                    msg.contains("start"),
                    "unexpected validation message: {}",
                    msg
                );
            }
            other => panic!("expected validation error, got {}", other),
        }
    }
}
