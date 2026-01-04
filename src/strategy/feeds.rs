//! Data Feed Manager
//!
//! Coordinates data feeds from Binance and Polymarket, converting their
//! updates to MarketUpdate events for the StrategyManager.

use std::sync::Arc;
use std::collections::HashMap;
use chrono::Utc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::adapters::{BinanceWebSocket, PolymarketWebSocket, PolymarketClient};
use crate::error::Result;
use super::manager::StrategyManager;
use super::traits::{MarketUpdate, DataFeed};

/// Manages data feeds and routes updates to StrategyManager
pub struct DataFeedManager {
    /// Reference to strategy manager
    manager: Arc<StrategyManager>,
    /// Binance WebSocket (optional)
    binance_ws: Option<Arc<BinanceWebSocket>>,
    /// Polymarket WebSocket (optional)
    polymarket_ws: Option<Arc<PolymarketWebSocket>>,
    /// Polymarket client for event discovery
    pm_client: Option<Arc<PolymarketClient>>,
    /// Token to event mapping for Polymarket
    token_events: Arc<RwLock<HashMap<String, EventMapping>>>,
    /// Active feeds
    active_feeds: Arc<RwLock<Vec<DataFeed>>>,
}

/// Mapping from token to event info
#[derive(Debug, Clone)]
struct EventMapping {
    event_id: String,
    series_id: String,
    is_up_token: bool,
}

impl DataFeedManager {
    /// Create a new DataFeedManager
    pub fn new(manager: Arc<StrategyManager>) -> Self {
        Self {
            manager,
            binance_ws: None,
            polymarket_ws: None,
            pm_client: None,
            token_events: Arc::new(RwLock::new(HashMap::new())),
            active_feeds: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Configure Binance feed for given symbols
    pub fn with_binance(mut self, symbols: Vec<String>) -> Self {
        if !symbols.is_empty() {
            self.binance_ws = Some(Arc::new(BinanceWebSocket::new(symbols)));
        }
        self
    }

    /// Configure Polymarket feed
    pub fn with_polymarket(mut self, ws: PolymarketWebSocket, client: PolymarketClient) -> Self {
        self.polymarket_ws = Some(Arc::new(ws));
        self.pm_client = Some(Arc::new(client));
        self
    }

    /// Start all configured data feeds
    pub async fn start(&self) -> Result<()> {
        info!("Starting data feed manager");

        // Start Binance feed if configured
        if let Some(ref binance_ws) = self.binance_ws {
            let manager = self.manager.clone();
            let mut rx = binance_ws.subscribe();

            tokio::spawn(async move {
                info!("Binance price feed started");
                while let Ok(update) = rx.recv().await {
                    let market_update = MarketUpdate::BinancePrice {
                        symbol: update.symbol,
                        price: update.price,
                        timestamp: Utc::now(),
                    };
                    manager.send_market_update(market_update);
                }
                warn!("Binance price feed ended");
            });

            // Start the WebSocket connection
            let ws = binance_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = ws.run().await {
                    error!("Binance WebSocket error: {}", e);
                }
            });
        }

        // Start Polymarket feed if configured
        if let Some(ref pm_ws) = self.polymarket_ws {
            let manager = self.manager.clone();
            let mut rx = pm_ws.subscribe_updates();

            tokio::spawn(async move {
                info!("Polymarket quote feed started");
                while let Ok(update) = rx.recv().await {
                    let market_update = MarketUpdate::PolymarketQuote {
                        token_id: update.token_id,
                        side: update.side,
                        quote: update.quote,
                        timestamp: Utc::now(),
                    };
                    manager.send_market_update(market_update);
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

    /// Discover events from a series and notify strategies
    pub async fn discover_series_events(&self, series_id: &str) -> Result<Vec<String>> {
        let mut token_ids = Vec::new();

        if let Some(ref client) = self.pm_client {
            match client.get_all_active_events(series_id).await {
                Ok(events) => {
                    let event_count = events.len();
                    for event in events {
                        // Get tokens for this event
                        if let Ok(event_details) = client.get_event_details(&event.id).await {
                            for market in event_details.markets {
                                // Get token IDs from tokens field
                                let tokens: Vec<String> = market.tokens
                                    .as_ref()
                                    .map(|t| t.iter().map(|tok| tok.token_id.clone()).collect())
                                    .unwrap_or_default();

                                if tokens.len() >= 2 {
                                    // Notify strategies of event discovery
                                    let end_time = event.end_date
                                        .as_ref()
                                        .and_then(|d| d.parse().ok())
                                        .unwrap_or_else(Utc::now);

                                    let update = MarketUpdate::EventDiscovered {
                                        event_id: event.id.clone(),
                                        series_id: series_id.to_string(),
                                        up_token: tokens.get(0).cloned().unwrap_or_default(),
                                        down_token: tokens.get(1).cloned().unwrap_or_default(),
                                        end_time,
                                    };
                                    self.manager.send_market_update(update);

                                    // Collect token IDs
                                    token_ids.extend(tokens);
                                }
                            }
                        }
                    }
                    info!("Discovered {} events from series {}", event_count, series_id);
                }
                Err(e) => {
                    warn!("Failed to fetch events for series {}: {}", series_id, e);
                }
            }
        }

        Ok(token_ids)
    }

    /// Start feeds based on strategy requirements
    pub async fn start_for_feeds(&self, feeds: Vec<DataFeed>) -> Result<Vec<String>> {
        let mut all_tokens = Vec::new();

        for feed in feeds {
            match feed {
                DataFeed::BinanceSpot { symbols } => {
                    if self.binance_ws.is_some() {
                        info!("Starting Binance feed for: {:?}", symbols);
                        // Binance WS is already configured with symbols in constructor
                    }
                }
                DataFeed::PolymarketEvents { series_ids } => {
                    for series_id in series_ids {
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

        // Subscribe to all discovered tokens
        if !all_tokens.is_empty() {
            self.subscribe_tokens(all_tokens.clone()).await?;
        }

        Ok(all_tokens)
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

    #[test]
    fn test_feed_builder() {
        let builder = DataFeedBuilder::new()
            .with_symbols(vec!["BTCUSDT".into(), "ETHUSDT".into()])
            .with_series(vec!["10192".into()]);

        let binance = builder.build_binance();
        assert!(binance.is_some());
    }
}
