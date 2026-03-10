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
use tracing::{debug, error, info, warn};

use super::manager::StrategyManager;
use super::traits::{DataFeed, KlineBar, MarketUpdate};
use crate::adapters::{
    BinanceKlineWebSocket, BinanceWebSocket, PolymarketClient, PolymarketWebSocket,
};
use crate::collector::{BinanceDepthStream, BinanceKlineClient};
use crate::error::Result;

mod polymarket_events;

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
    data_plane: Option<Arc<crate::platform::PlatformDataPlane>>,
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
        dp: Arc<crate::platform::PlatformDataPlane>,
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

    async fn backfill_binance_klines(&self) -> Result<()> {
        let limit = self.binance_kline_backfill_limit;
        if limit == 0 {
            return Ok(());
        }

        if self.binance_kline_symbols.is_empty() || self.binance_kline_intervals.is_empty() {
            return Ok(());
        }

        let now = Utc::now();
        info!(
            "Backfilling Binance klines: symbols={:?} intervals={:?} limit={}",
            self.binance_kline_symbols, self.binance_kline_intervals, limit
        );

        let client = BinanceKlineClient::new();
        let mut sent: u64 = 0;

        for sym in &self.binance_kline_symbols {
            for interval in &self.binance_kline_intervals {
                let mut klines = match client.fetch_klines(sym, interval, limit).await {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(
                            "Binance kline backfill failed for {} {}: {}",
                            sym, interval, e
                        );
                        continue;
                    }
                };

                // REST can include the currently open candle; only seed closed candles.
                klines.retain(|k| k.close_time <= now);
                klines.sort_by_key(|k| k.open_time);

                if let Some(last) = klines.last() {
                    let mut map = self.binance_kline_last_close.write().await;
                    map.entry(sym.clone())
                        .or_default()
                        .insert(interval.clone(), last.close_time);
                }

                // Persist klines to DB for training scripts (if pool available)
                if let Some(ref pool) = self.metadata_pool {
                    match BinanceKlineClient::save_klines_to_db(pool, sym, interval, &klines).await
                    {
                        Ok(n) if n > 0 => {
                            info!("Persisted {} klines for {} {} to DB", n, sym, interval);
                        }
                        Err(e) => {
                            debug!("kline DB persist skipped for {} {}: {}", sym, interval, e);
                        }
                        _ => {}
                    }
                }

                info!(
                    "Backfilled {} klines for {} {}",
                    klines.len(),
                    sym,
                    interval
                );

                let sym_s = sym.clone();
                let interval_s = interval.clone();
                for k in klines {
                    let bar = KlineBar {
                        open_time: k.open_time,
                        close_time: k.close_time,
                        open: k.open,
                        high: k.high,
                        low: k.low,
                        close: k.close,
                        volume: k.volume,
                        is_closed: true,
                    };

                    let market_update = MarketUpdate::BinanceKline {
                        symbol: sym_s.clone(),
                        interval: interval_s.clone(),
                        kline: bar,
                        timestamp: k.close_time,
                    };
                    self.manager.send_market_update(market_update);
                    sent = sent.saturating_add(1);

                    // Let strategy tasks drain the broadcast channel (avoid lag/drops).
                    if sent % 50 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
            }
        }

        info!("Binance kline backfill complete ({} updates)", sent);
        Ok(())
    }

    /// Start all configured data feeds
    pub async fn start(&self) -> Result<()> {
        info!("Starting data feed manager");

        // Start Binance feed if configured
        if let Some(ref binance_ws) = self.binance_ws {
            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = binance_ws.subscribe();

            tokio::spawn(async move {
                info!("Binance price feed started");
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            let market_update = MarketUpdate::BinancePrice {
                                symbol: update.symbol,
                                price: update.price,
                                timestamp: Utc::now(),
                            };
                            manager.send_market_update(market_update);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Binance price feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!("Binance price feed ended");
                            break;
                        }
                    }
                }
            });

            if self.data_plane.is_none() {
                // Start the WebSocket connection
                let ws = binance_ws.clone();
                tokio::spawn(async move {
                    if let Err(e) = ws.run().await {
                        error!("Binance WebSocket error: {}", e);
                    }
                });
            }
        }

        // Start Binance kline feed if configured
        if let Some(ref binance_ws) = self.binance_kline_ws {
            // Warm-start pattern memory strategies with a chunk of historical klines.
            // This happens before Polymarket discovery/subscription in the CLI flow, so
            // strategies won't place orders based on backfill.
            self.backfill_binance_klines().await?;

            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = binance_ws.subscribe();
            let last_close = self.binance_kline_last_close.clone();

            tokio::spawn(async move {
                info!("Binance kline feed started");
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            // Skip duplicates from backfill overlap or WS reconnect replay.
                            let should_skip = {
                                let map = last_close.read().await;
                                map.get(&update.symbol)
                                    .and_then(|m| m.get(&update.interval))
                                    .map(|t| update.kline.close_time <= *t)
                                    .unwrap_or(false)
                            };
                            if should_skip {
                                continue;
                            }

                            {
                                let mut map = last_close.write().await;
                                map.entry(update.symbol.clone())
                                    .or_default()
                                    .insert(update.interval.clone(), update.kline.close_time);
                            }

                            let bar = KlineBar {
                                open_time: update.kline.open_time,
                                close_time: update.kline.close_time,
                                open: update.kline.open,
                                high: update.kline.high,
                                low: update.kline.low,
                                close: update.kline.close,
                                volume: update.kline.volume,
                                is_closed: update.kline.is_closed,
                            };

                            let market_update = MarketUpdate::BinanceKline {
                                symbol: update.symbol,
                                interval: update.interval,
                                kline: bar,
                                timestamp: update.event_time,
                            };
                            manager.send_market_update(market_update);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Binance kline feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!("Binance kline feed ended");
                            break;
                        }
                    }
                }
            });

            if self.data_plane.is_none() {
                let ws = binance_ws.clone();
                tokio::spawn(async move {
                    if let Err(e) = ws.run().await {
                        error!("Binance kline WebSocket error: {}", e);
                    }
                });
            }
        }

        // Start Polymarket feed if configured
        if let Some(ref pm_ws) = self.polymarket_ws {
            let manager = self.manager.clone();
            let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
            let mut rx = pm_ws.subscribe_updates();

            tokio::spawn(async move {
                info!("Polymarket quote feed started - waiting for quotes");
                let mut quote_count = 0u64;
                loop {
                    match rx.recv().await {
                        Ok(update) => {
                            quote_count += 1;
                            if quote_count <= 10 || quote_count % 5000 == 0 {
                                info!(
                                    "Feed forwarding quote #{}: {} {:?} bid={:?} ask={:?}",
                                    quote_count,
                                    &update.token_id[..8.min(update.token_id.len())],
                                    update.side,
                                    update.quote.best_bid,
                                    update.quote.best_ask
                                );
                            } else {
                                debug!(
                                    "Feed forwarding quote #{}: {} {:?} bid={:?} ask={:?}",
                                    quote_count,
                                    &update.token_id[..8.min(update.token_id.len())],
                                    update.side,
                                    update.quote.best_bid,
                                    update.quote.best_ask
                                );
                            }
                            let market_update = MarketUpdate::PolymarketQuote {
                                token_id: update.token_id,
                                side: update.side,
                                quote: update.quote,
                                timestamp: Utc::now(),
                            };
                            manager.send_market_update(market_update);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Quote feed lagged by {} messages", n);
                            if let Some(ref f) = freshness {
                                f.record_broadcast_lag(n as u64);
                            }
                            continue;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            warn!("Quote feed recv error: channel closed");
                            break;
                        }
                    }
                }
                warn!("Polymarket quote feed ended");
            });
        }

        Ok(())
    }

    /// Subscribe to tokens for a set of events
    pub async fn subscribe_tokens(&self, token_ids: Vec<String>) -> Result<()> {
        if let Some(ref pm_ws) = self.polymarket_ws {
            info!("Subscribing to {} Polymarket tokens", token_ids.len());

            if self.data_plane.is_some() {
                debug!(
                    "Polymarket WS run is managed by PlatformDataPlane; skipping local ws.run()"
                );
                pm_ws.request_resubscribe();
                return Ok(());
            }

            // Start WebSocket with tokens
            let ws = pm_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = ws.run(token_ids).await {
                    error!("Polymarket WebSocket error: {}", e);
                }
            });
        }
        Ok(())
    }

    /// Start feeds based on strategy requirements
    pub async fn start_for_feeds(&self, feeds: Vec<DataFeed>) -> Result<Vec<String>> {
        let mut all_tokens = Vec::new();
        let mut series_ids_to_refresh: Vec<String> = Vec::new();
        let mut binance_l2_symbols: Vec<String> = Vec::new();

        for feed in feeds {
            match feed {
                DataFeed::BinanceSpot { symbols } => {
                    if self.binance_ws.is_some() {
                        info!("Starting Binance feed for: {:?}", symbols);
                        // Binance WS is already configured with symbols in constructor
                    }
                    binance_l2_symbols.extend(symbols);
                }
                DataFeed::BinanceKlines {
                    symbols,
                    intervals,
                    closed_only,
                } => {
                    if self.binance_kline_ws.is_some() {
                        info!(
                            "Starting Binance kline feed for: symbols={:?} intervals={:?} closed_only={}",
                            symbols, intervals, closed_only
                        );
                    }
                }
                DataFeed::PolymarketEvents { series_ids } => {
                    for series_id in series_ids {
                        series_ids_to_refresh.push(series_id.clone());
                        let tokens = self.discover_series_events(&series_id).await?;
                        all_tokens.extend(tokens);
                    }
                }
                DataFeed::PolymarketQuotes { tokens } => {
                    // Direct token subscription
                    all_tokens.extend(tokens);
                }
                DataFeed::Tick { interval_ms } => {
                    // Tick is handled by StrategyManager's event loop
                    debug!("Tick feed configured: {}ms", interval_ms);
                }
            }
        }

        self.ensure_binance_l2_feed_started(binance_l2_symbols)
            .await;

        // Subscribe to Polymarket tokens.
        //
        // IMPORTANT: for rotating series feeds, we pass an empty seed list and rely on the ws'
        // internal token mapping. This keeps the subscription set bounded as markets rotate.
        if self.polymarket_ws.is_some() {
            if series_ids_to_refresh.is_empty() {
                // Direct token subscription (non-rotating). Use seed tokens.
                if !all_tokens.is_empty() {
                    self.subscribe_tokens(all_tokens.clone()).await?;
                }
            } else {
                self.subscribe_tokens(Vec::new()).await?;
            }
        }

        // Start periodic refresh for Polymarket series (keeps token set rotating).
        if !series_ids_to_refresh.is_empty() {
            self.spawn_polymarket_refresh(series_ids_to_refresh).await;
        }

        Ok(all_tokens)
    }

    async fn ensure_binance_l2_feed_started(&self, mut symbols: Vec<String>) {
        if !l2_feed_enabled() {
            return;
        }

        symbols.retain(|s| !s.trim().is_empty());
        symbols.sort();
        symbols.dedup();
        if symbols.is_empty() {
            return;
        }

        {
            let mut started = self.binance_l2_started.write().await;
            if *started {
                return;
            }
            *started = true;
        }

        let manager = self.manager.clone();
        let freshness = self.data_plane.as_ref().map(|dp| dp.freshness());
        let depth_ws = Arc::new(match freshness.clone() {
            Some(f) => BinanceDepthStream::new(symbols.clone()).with_freshness(f),
            None => BinanceDepthStream::new(symbols.clone()),
        });
        let mut rx = depth_ws.subscribe();

        tokio::spawn(async move {
            info!("Binance L2 depth feed started for {:?}", symbols);
            loop {
                match rx.recv().await {
                    Ok(update) => {
                        let market_update = MarketUpdate::BinanceL2 {
                            symbol: update.symbol,
                            obi_1: update.snapshot.obi_1,
                            obi_2: update.snapshot.obi_2,
                            obi_3: update.snapshot.obi_3,
                            obi_5: update.snapshot.obi_5,
                            obi_10: update.snapshot.obi_10,
                            obi_20: update.snapshot.obi_20,
                            bid_volume_5: update.snapshot.bid_volume_5,
                            ask_volume_5: update.snapshot.ask_volume_5,
                            spread_bps: update.snapshot.spread_bps,
                            timestamp: update.snapshot.timestamp,
                        };
                        manager.send_market_update(market_update);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Binance L2 depth feed lagged by {} messages", n);
                        if let Some(ref f) = freshness {
                            f.record_broadcast_lag(n as u64);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        warn!("Binance L2 depth feed ended");
                        break;
                    }
                }
            }
        });

        tokio::spawn(async move {
            if let Err(e) = depth_ws.run().await {
                error!("Binance L2 depth WebSocket error: {}", e);
            }
        });
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
    use crate::platform::{DataPlaneConfig, DataPlaneFreshness, PlatformDataPlane};
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

    #[test]
    fn test_from_data_plane_reuses_singleton_adapters() {
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
