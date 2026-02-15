use crate::adapters::{PolymarketClient, PolymarketWebSocket, PostgresStore};
use crate::domain::{Round, Tick};
use crate::error::Result;
use chrono::{Duration, Utc};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration as TokioDuration};
use tracing::{debug, error, info, warn};

/// Data collector for background market data gathering
pub struct DataCollector {
    client: PolymarketClient,
    store: PostgresStore,
    ws: Arc<PolymarketWebSocket>,
    market_slug: String,
    /// Current round being tracked
    current_round: Arc<RwLock<Option<Round>>>,
    /// Tick buffer for batch inserts
    tick_buffer: Arc<RwLock<Vec<Tick>>>,
    /// Buffer flush interval in seconds
    flush_interval_secs: u64,
}

impl DataCollector {
    /// Create a new data collector
    pub fn new(
        client: PolymarketClient,
        store: PostgresStore,
        ws: Arc<PolymarketWebSocket>,
        market_slug: &str,
    ) -> Self {
        Self {
            client,
            store,
            ws,
            market_slug: market_slug.to_string(),
            current_round: Arc::new(RwLock::new(None)),
            tick_buffer: Arc::new(RwLock::new(Vec::new())),
            flush_interval_secs: 5,
        }
    }

    /// Get the current round
    pub async fn current_round(&self) -> Option<Round> {
        self.current_round.read().await.clone()
    }

    /// Start the data collector background tasks
    pub async fn start(&self) -> Result<()> {
        info!("Starting data collector for {}", self.market_slug);

        // Spawn round discovery task
        let round_task = self.spawn_round_discovery();

        // Spawn tick persistence task
        let tick_task = self.spawn_tick_persistence();

        // Spawn quote collection task
        let quote_task = self.spawn_quote_collection();

        // Wait for all tasks (they should run forever)
        tokio::select! {
            r = round_task => {
                error!("Round discovery task ended: {:?}", r);
            }
            r = tick_task => {
                error!("Tick persistence task ended: {:?}", r);
            }
            r = quote_task => {
                error!("Quote collection task ended: {:?}", r);
            }
        }

        Ok(())
    }

    /// Spawn round discovery task
    fn spawn_round_discovery(&self) -> tokio::task::JoinHandle<()> {
        let client = self.client.clone();
        let market_slug = self.market_slug.clone();
        let current_round = Arc::clone(&self.current_round);
        let store = self.store.clone();
        let ws = Arc::clone(&self.ws);

        tokio::spawn(async move {
            let mut check_interval = interval(TokioDuration::from_secs(10));

            loop {
                check_interval.tick().await;

                match Self::discover_round(&client, &market_slug).await {
                    Ok(Some(round)) => {
                        let mut current = current_round.write().await;

                        // Check if this is a new round
                        let is_new = match &*current {
                            Some(r) => r.slug != round.slug,
                            None => true,
                        };

                        if is_new {
                            info!("New round discovered: {}", round.slug);

                            // Register tokens with WebSocket
                            ws.register_tokens(&round.up_token_id, &round.down_token_id)
                                .await;

                            // Persist to database
                            if let Err(e) = store.upsert_round(&round).await {
                                error!("Failed to persist round: {}", e);
                            }

                            *current = Some(round);
                        }
                    }
                    Ok(None) => {
                        debug!("No active round found");
                        *current_round.write().await = None;
                    }
                    Err(e) => {
                        warn!("Failed to discover round: {}", e);
                    }
                }
            }
        })
    }

    /// Discover the current active round
    async fn discover_round(client: &PolymarketClient, market_slug: &str) -> Result<Option<Round>> {
        // Search for the market
        let markets = client.search_markets(market_slug).await?;

        for market in markets {
            if !market.active {
                continue;
            }

            // Get full market info
            let market_info = client.get_market(&market.condition_id).await?;

            if market_info.active && !market_info.closed {
                // Parse tokens (usually "Yes" and "No" or similar)
                let mut up_token = None;
                let mut down_token = None;

                for token in &market_info.tokens {
                    match token.outcome.to_lowercase().as_str() {
                        "yes" | "up" => up_token = Some(token.token_id.clone()),
                        "no" | "down" => down_token = Some(token.token_id.clone()),
                        _ => {}
                    }
                }

                if let (Some(up), Some(down)) = (up_token, down_token) {
                    // Parse end time
                    let end_time = market_info
                        .end_date_iso
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or_else(|| Utc::now() + Duration::minutes(15));

                    // Estimate start time (15 minutes before end)
                    let start_time = end_time - Duration::minutes(15);

                    return Ok(Some(Round {
                        id: None,
                        slug: market.slug.unwrap_or_else(|| market.condition_id.clone()),
                        up_token_id: up,
                        down_token_id: down,
                        start_time,
                        end_time,
                        outcome: None,
                    }));
                }
            }
        }

        Ok(None)
    }

    /// Spawn tick persistence task
    fn spawn_tick_persistence(&self) -> tokio::task::JoinHandle<()> {
        let store = self.store.clone();
        let tick_buffer = Arc::clone(&self.tick_buffer);
        let flush_interval = self.flush_interval_secs;

        tokio::spawn(async move {
            let mut flush_interval = interval(TokioDuration::from_secs(flush_interval));

            loop {
                flush_interval.tick().await;

                let ticks: Vec<Tick> = {
                    let mut buffer = tick_buffer.write().await;
                    std::mem::take(&mut *buffer)
                };

                if !ticks.is_empty() {
                    debug!("Flushing {} ticks to database", ticks.len());
                    if let Err(e) = store.insert_ticks(&ticks).await {
                        error!("Failed to persist ticks: {}", e);
                    }
                }
            }
        })
    }

    /// Spawn quote collection task
    fn spawn_quote_collection(&self) -> tokio::task::JoinHandle<()> {
        let ws = Arc::clone(&self.ws);
        let current_round = Arc::clone(&self.current_round);
        let tick_buffer = Arc::clone(&self.tick_buffer);

        tokio::spawn(async move {
            let mut updates = ws.subscribe_updates();

            loop {
                match updates.recv().await {
                    Ok(update) => {
                        // Get round ID
                        let round_id = {
                            let round = current_round.read().await;
                            round.as_ref().and_then(|r| r.id)
                        };

                        if let Some(round_id) = round_id {
                            // Create tick from quote
                            let tick = Tick {
                                id: None,
                                round_id,
                                timestamp: update.quote.timestamp,
                                side: update.side,
                                best_bid: update.quote.best_bid,
                                best_ask: update.quote.best_ask,
                                bid_size: update.quote.bid_size,
                                ask_size: update.quote.ask_size,
                            };

                            // Add to buffer
                            tick_buffer.write().await.push(tick);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Quote collector lagged by {} messages", n);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        error!("Quote update channel closed");
                        break;
                    }
                }
            }
        })
    }
}
