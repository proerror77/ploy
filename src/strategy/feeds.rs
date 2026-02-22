//! Data Feed Manager
//!
//! Coordinates data feeds from Binance and Polymarket, converting their
//! updates to MarketUpdate events for the StrategyManager.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::manager::StrategyManager;
use super::traits::{DataFeed, KlineBar, MarketUpdate};
use crate::adapters::{
    BinanceKlineWebSocket, BinanceWebSocket, PolymarketClient, PolymarketWebSocket,
};
use crate::collector::BinanceKlineClient;
use crate::error::Result;

const MAX_EVENTS_PER_SERIES: usize = 6;
const POLYMARKET_REFRESH_SECS: u64 = 30;

fn infer_symbol_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("bitcoin") || lower.contains("btc") {
        Some("BTCUSDT")
    } else if lower.contains("ethereum") || lower.contains("eth") {
        Some("ETHUSDT")
    } else if lower.contains("solana") || lower.contains("sol") {
        Some("SOLUSDT")
    } else if lower.contains("ripple") || lower.contains("xrp") {
        Some("XRPUSDT")
    } else {
        None
    }
}

fn infer_horizon_from_text(text: &str) -> Option<&'static str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("15m")
        || lower.contains("15-minute")
        || lower.contains("15 minute")
        || lower.contains("15min")
        || lower.contains("15 min")
    {
        Some("15m")
    } else if lower.contains("5m")
        || lower.contains("5-minute")
        || lower.contains("5 minute")
        || lower.contains("5min")
        || lower.contains("5 min")
    {
        Some("5m")
    } else {
        None
    }
}

fn apply_dimension_candidate(
    text: &str,
    symbol: &mut Option<String>,
    horizon: &mut Option<String>,
) {
    if symbol.is_none() {
        if let Some(s) = infer_symbol_from_text(text) {
            *symbol = Some(s.to_string());
        }
    }
    if horizon.is_none() {
        if let Some(h) = infer_horizon_from_text(text) {
            *horizon = Some(h.to_string());
        }
    }
}

fn infer_symbol_horizon_from_event(
    details: &crate::adapters::polymarket_clob::GammaEventInfo,
) -> (Option<String>, Option<String>) {
    let mut symbol: Option<String> = None;
    let mut horizon: Option<String> = None;

    if let Some(slug) = details.slug.as_deref() {
        apply_dimension_candidate(slug, &mut symbol, &mut horizon);
    }
    if let Some(title) = details.title.as_deref() {
        apply_dimension_candidate(title, &mut symbol, &mut horizon);
    }

    for market in &details.markets {
        if let Some(group_title) = market.group_item_title.as_deref() {
            apply_dimension_candidate(group_title, &mut symbol, &mut horizon);
        }
        if let Some(question) = market.question.as_deref() {
            apply_dimension_candidate(question, &mut symbol, &mut horizon);
        }
        if symbol.is_some() && horizon.is_some() {
            break;
        }
    }

    (symbol, horizon)
}

fn parse_rfc3339_utc(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

async fn upsert_pm_market_metadata(
    pool: Option<&PgPool>,
    details: &crate::adapters::polymarket_clob::GammaEventInfo,
    price_to_beat: rust_decimal::Decimal,
    end_time: DateTime<Utc>,
) -> Result<()> {
    let Some(pool) = pool else {
        return Ok(());
    };

    let market_slug = details.slug.clone().unwrap_or_else(|| details.id.clone());
    let start_time = parse_rfc3339_utc(details.start_time.as_deref());
    let (symbol, horizon) = infer_symbol_horizon_from_event(details);
    let raw_market: Value = serde_json::to_value(details).unwrap_or_else(|_| Value::Null);

    // Keep dataset clean for sequence training alignment: crypto symbols + 5m/15m only.
    let (Some(symbol), Some(horizon)) = (symbol, horizon) else {
        return Ok(());
    };

    sqlx::query(
        r#"
        INSERT INTO pm_market_metadata (
            market_slug, price_to_beat, start_time, end_time, horizon, symbol, raw_market, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, NOW())
        ON CONFLICT (market_slug) DO UPDATE SET
            price_to_beat = EXCLUDED.price_to_beat,
            start_time = COALESCE(EXCLUDED.start_time, pm_market_metadata.start_time),
            end_time = COALESCE(EXCLUDED.end_time, pm_market_metadata.end_time),
            horizon = COALESCE(EXCLUDED.horizon, pm_market_metadata.horizon),
            symbol = COALESCE(EXCLUDED.symbol, pm_market_metadata.symbol),
            raw_market = COALESCE(EXCLUDED.raw_market, pm_market_metadata.raw_market),
            updated_at = NOW()
        "#,
    )
    .bind(market_slug)
    .bind(price_to_beat)
    .bind(start_time)
    .bind(end_time)
    .bind(horizon)
    .bind(symbol)
    .bind(raw_market)
    .execute(pool)
    .await?;

    Ok(())
}

fn parse_price_from_question(question: &str) -> Option<rust_decimal::Decimal> {
    // Intentionally strict: avoid mis-parsing dates/times in "Up or Down" titles.
    // Only parse when the string contains a clear price marker like `$` or `↑/↓`.
    let marker_idx = question.char_indices().find_map(|(i, c)| match c {
        '$' | '↑' | '↓' => Some(i + c.len_utf8()),
        _ => None,
    })?;

    let tail = &question[marker_idx..];
    let cleaned: String = tail
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
        .filter(|c| *c != ',')
        .collect();

    if cleaned.is_empty() {
        return None;
    }

    cleaned.parse::<rust_decimal::Decimal>().ok()
}

/// Manages data feeds and routes updates to StrategyManager
pub struct DataFeedManager {
    /// Reference to strategy manager
    manager: Arc<StrategyManager>,
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
    token_events: Arc<RwLock<HashMap<String, EventMapping>>>,
    /// Active feeds
    active_feeds: Arc<RwLock<Vec<DataFeed>>>,
    /// Latest discovered events per series (bounded, for refresh + token reconciliation)
    series_events: Arc<RwLock<HashMap<String, HashMap<String, DiscoveredEvent>>>>,
    /// Optional DB pool used to persist normalized market metadata for model training.
    metadata_pool: Option<Arc<PgPool>>,
}

/// Mapping from token to event info
#[derive(Debug, Clone)]
struct EventMapping {
    event_id: String,
    series_id: String,
    is_up_token: bool,
}

#[derive(Debug, Clone)]
struct DiscoveredEvent {
    event_id: String,
    series_id: String,
    up_token: String,
    down_token: String,
    end_time: chrono::DateTime<Utc>,
    price_to_beat: Option<rust_decimal::Decimal>,
    title: Option<String>,
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

        // Start Binance kline feed if configured
        if let Some(ref binance_ws) = self.binance_kline_ws {
            // Warm-start pattern memory strategies with a chunk of historical klines.
            // This happens before Polymarket discovery/subscription in the CLI flow, so
            // strategies won't place orders based on backfill.
            self.backfill_binance_klines().await?;

            let manager = self.manager.clone();
            let mut rx = binance_ws.subscribe();
            let last_close = self.binance_kline_last_close.clone();

            tokio::spawn(async move {
                info!("Binance kline feed started");
                while let Ok(update) = rx.recv().await {
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
                warn!("Binance kline feed ended");
            });

            let ws = binance_ws.clone();
            tokio::spawn(async move {
                if let Err(e) = ws.run().await {
                    error!("Binance kline WebSocket error: {}", e);
                }
            });
        }

        // Start Polymarket feed if configured
        if let Some(ref pm_ws) = self.polymarket_ws {
            let manager = self.manager.clone();
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
                        Err(e) => {
                            warn!("Quote feed recv error: {:?}", e);
                            // Continue on lagged, break on closed
                            if matches!(e, tokio::sync::broadcast::error::RecvError::Closed) {
                                break;
                            }
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
    /// Only fetches details for the next few upcoming events to minimize API calls
    pub async fn discover_series_events(&self, series_id: &str) -> Result<Vec<String>> {
        use crate::domain::Side;

        let mut token_ids = Vec::new();

        if let Some(ref client) = self.pm_client {
            match client.get_all_active_events(series_id).await {
                Ok(mut events) => {
                    let total_events = events.len();

                    // /series/{id} returns lightweight event summaries without markets/tokens.
                    // Filter to near-future events and fetch full details for a small subset.
                    let now = Utc::now();
                    let min_end_time = now + chrono::Duration::seconds(30);
                    let max_end_time = now + chrono::Duration::minutes(60);

                    let mut candidates: Vec<(chrono::DateTime<Utc>, String)> = Vec::new();
                    for e in &events {
                        let Some(end_str) = e.end_date.as_ref() else {
                            continue;
                        };
                        let Ok(end) = chrono::DateTime::parse_from_rfc3339(end_str)
                            .map(|dt| dt.with_timezone(&Utc))
                        else {
                            continue;
                        };
                        if end <= min_end_time || end > max_end_time {
                            continue;
                        }
                        candidates.push((end, e.id.clone()));
                    }

                    candidates.sort_by(|a, b| a.0.cmp(&b.0));

                    let mut discovered: HashMap<String, DiscoveredEvent> = HashMap::new();
                    for (_end, event_id) in candidates.into_iter().take(MAX_EVENTS_PER_SERIES) {
                        let details = match client.get_event_details(&event_id).await {
                            Ok(d) => d,
                            Err(e) => {
                                debug!("Failed to fetch event details for {}: {}", event_id, e);
                                continue;
                            }
                        };

                        let end_time = details
                            .end_date
                            .as_ref()
                            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(Utc::now);

                        let mut up_token: Option<String> = None;
                        let mut down_token: Option<String> = None;

                        let mut price_to_beat: Option<rust_decimal::Decimal> = details
                            .title
                            .as_ref()
                            .and_then(|t| parse_price_from_question(t));
                        let mut title: Option<String> = details.title.clone();

                        for market in &details.markets {
                            if price_to_beat.is_none() {
                                if let Some(s) = market.group_item_title.as_ref() {
                                    price_to_beat = parse_price_from_question(s);
                                }
                            }
                            if price_to_beat.is_none() {
                                if let Some(s) = market.question.as_ref() {
                                    price_to_beat = parse_price_from_question(s);
                                    if title.is_none() {
                                        title = Some(s.clone());
                                    }
                                }
                            }

                            if let Some(ids_str) = market.clob_token_ids.as_ref() {
                                if let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_str) {
                                    if ids.len() >= 2 {
                                        up_token = Some(ids[0].clone());
                                        down_token = Some(ids[1].clone());
                                        break;
                                    }
                                }
                            }

                            if up_token.is_none() || down_token.is_none() {
                                if let Some(tokens) = market.tokens.as_ref() {
                                    let up = tokens.iter().find(|t| {
                                        let outcome = t.outcome.to_lowercase();
                                        outcome.contains("up")
                                            || outcome == "yes"
                                            || outcome.starts_with("↑")
                                    });
                                    let down = tokens.iter().find(|t| {
                                        let outcome = t.outcome.to_lowercase();
                                        outcome.contains("down")
                                            || outcome == "no"
                                            || outcome.starts_with("↓")
                                    });
                                    if let (Some(u), Some(d)) = (up, down) {
                                        up_token = Some(u.token_id.clone());
                                        down_token = Some(d.token_id.clone());
                                        break;
                                    }
                                }
                            }
                        }

                        let (Some(up_token), Some(down_token)) = (up_token, down_token) else {
                            continue;
                        };
                        let Some(price_to_beat) = price_to_beat else {
                            // Keep only events with explicit threshold to align labels/features.
                            continue;
                        };

                        if let Err(e) = upsert_pm_market_metadata(
                            self.metadata_pool.as_deref(),
                            &details,
                            price_to_beat,
                            end_time,
                        )
                        .await
                        {
                            warn!(
                                series_id = %series_id,
                                event_id = %details.id,
                                error = %e,
                                "failed to upsert pm_market_metadata"
                            );
                        }

                        if let Some(ref pm_ws) = self.polymarket_ws {
                            pm_ws.register_token(&up_token, Side::Up).await;
                            pm_ws.register_token(&down_token, Side::Down).await;
                        }

                        let ev = DiscoveredEvent {
                            event_id: details.id.clone(),
                            series_id: series_id.to_string(),
                            up_token: up_token.clone(),
                            down_token: down_token.clone(),
                            end_time,
                            price_to_beat: Some(price_to_beat),
                            title: title.clone(),
                        };

                        discovered.insert(details.id.clone(), ev);
                        token_ids.push(up_token);
                        token_ids.push(down_token);
                    }

                    // Diff vs previous and notify strategies.
                    {
                        let mut series_events = self.series_events.write().await;
                        let prev = series_events.entry(series_id.to_string()).or_default();

                        // Expire removed.
                        let removed: Vec<String> = prev
                            .keys()
                            .filter(|id| !discovered.contains_key(*id))
                            .cloned()
                            .collect();
                        for event_id in removed {
                            prev.remove(&event_id);
                            self.manager
                                .send_market_update(MarketUpdate::EventExpired { event_id });
                        }

                        // Discover new/changed.
                        for (event_id, ev) in discovered {
                            let should_send = match prev.get(&event_id) {
                                None => true,
                                Some(old) => {
                                    old.up_token != ev.up_token
                                        || old.down_token != ev.down_token
                                        || old.end_time != ev.end_time
                                        || old.price_to_beat != ev.price_to_beat
                                }
                            };

                            if should_send {
                                let update = MarketUpdate::EventDiscovered {
                                    event_id: ev.event_id.clone(),
                                    series_id: ev.series_id.clone(),
                                    up_token: ev.up_token.clone(),
                                    down_token: ev.down_token.clone(),
                                    end_time: ev.end_time,
                                    price_to_beat: ev.price_to_beat,
                                    title: ev.title.clone(),
                                };
                                self.manager.send_market_update(update);
                            }

                            prev.insert(event_id, ev);
                        }
                    }

                    info!(
                        "Series {}: active={} kept={} subscribed_tokens={}",
                        series_id,
                        total_events,
                        token_ids.len() / 2,
                        token_ids.len()
                    );
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
        let mut series_ids_to_refresh: Vec<String> = Vec::new();

        for feed in feeds {
            match feed {
                DataFeed::BinanceSpot { symbols } => {
                    if self.binance_ws.is_some() {
                        info!("Starting Binance feed for: {:?}", symbols);
                        // Binance WS is already configured with symbols in constructor
                    }
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

    async fn spawn_polymarket_refresh(&self, series_ids: Vec<String>) {
        let Some(pm_client) = self.pm_client.clone() else {
            return;
        };
        let Some(pm_ws) = self.polymarket_ws.clone() else {
            return;
        };

        let manager = self.manager.clone();
        let series_events = self.series_events.clone();
        let metadata_pool = self.metadata_pool.clone();

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(POLYMARKET_REFRESH_SECS));
            loop {
                ticker.tick().await;

                for series_id in &series_ids {
                    // Fetch active events and keep only the nearest ones.
                    let Ok(mut events) = pm_client.get_all_active_events(series_id).await else {
                        continue;
                    };

                    // /series/{id} returns lightweight summaries; fetch details for only a small
                    // near-future subset.
                    let now = Utc::now();
                    let min_end_time = now + chrono::Duration::seconds(30);
                    let max_end_time = now + chrono::Duration::minutes(60);

                    let mut candidates: Vec<(chrono::DateTime<Utc>, String)> = Vec::new();
                    for e in &events {
                        let Some(end_str) = e.end_date.as_ref() else {
                            continue;
                        };
                        let Ok(end) = chrono::DateTime::parse_from_rfc3339(end_str)
                            .map(|dt| dt.with_timezone(&Utc))
                        else {
                            continue;
                        };
                        if end <= min_end_time || end > max_end_time {
                            continue;
                        }
                        candidates.push((end, e.id.clone()));
                    }

                    candidates.sort_by(|a, b| a.0.cmp(&b.0));

                    let mut discovered: HashMap<String, DiscoveredEvent> = HashMap::new();
                    for (_end, event_id) in candidates.into_iter().take(MAX_EVENTS_PER_SERIES) {
                        let details = match pm_client.get_event_details(&event_id).await {
                            Ok(d) => d,
                            Err(e) => {
                                debug!(
                                    "Failed to fetch event details for {} (series {}): {}",
                                    event_id, series_id, e
                                );
                                continue;
                            }
                        };

                        let end_time = details
                            .end_date
                            .as_ref()
                            .and_then(|d| chrono::DateTime::parse_from_rfc3339(d).ok())
                            .map(|dt| dt.with_timezone(&Utc))
                            .unwrap_or_else(Utc::now);

                        let mut up_token: Option<String> = None;
                        let mut down_token: Option<String> = None;

                        let mut price_to_beat: Option<rust_decimal::Decimal> = details
                            .title
                            .as_ref()
                            .and_then(|t| parse_price_from_question(t));
                        let mut title: Option<String> = details.title.clone();

                        for market in &details.markets {
                            if price_to_beat.is_none() {
                                if let Some(s) = market.group_item_title.as_ref() {
                                    price_to_beat = parse_price_from_question(s);
                                }
                            }
                            if price_to_beat.is_none() {
                                if let Some(s) = market.question.as_ref() {
                                    price_to_beat = parse_price_from_question(s);
                                    if title.is_none() {
                                        title = Some(s.clone());
                                    }
                                }
                            }

                            if let Some(ids_str) = market.clob_token_ids.as_ref() {
                                if let Ok(ids) = serde_json::from_str::<Vec<String>>(ids_str) {
                                    if ids.len() >= 2 {
                                        up_token = Some(ids[0].clone());
                                        down_token = Some(ids[1].clone());
                                        break;
                                    }
                                }
                            }

                            if up_token.is_none() || down_token.is_none() {
                                if let Some(tokens) = market.tokens.as_ref() {
                                    let up = tokens.iter().find(|t| {
                                        let outcome = t.outcome.to_lowercase();
                                        outcome.contains("up")
                                            || outcome == "yes"
                                            || outcome.starts_with("↑")
                                    });
                                    let down = tokens.iter().find(|t| {
                                        let outcome = t.outcome.to_lowercase();
                                        outcome.contains("down")
                                            || outcome == "no"
                                            || outcome.starts_with("↓")
                                    });
                                    if let (Some(u), Some(d)) = (up, down) {
                                        up_token = Some(u.token_id.clone());
                                        down_token = Some(d.token_id.clone());
                                        break;
                                    }
                                }
                            }
                        }

                        let (Some(up_token), Some(down_token)) = (up_token, down_token) else {
                            continue;
                        };
                        let Some(price_to_beat) = price_to_beat else {
                            continue;
                        };

                        if let Err(e) = upsert_pm_market_metadata(
                            metadata_pool.as_deref(),
                            &details,
                            price_to_beat,
                            end_time,
                        )
                        .await
                        {
                            warn!(
                                series_id = %series_id,
                                event_id = %details.id,
                                error = %e,
                                "failed to upsert pm_market_metadata"
                            );
                        }

                        pm_ws
                            .register_token(&up_token, crate::domain::Side::Up)
                            .await;
                        pm_ws
                            .register_token(&down_token, crate::domain::Side::Down)
                            .await;

                        discovered.insert(
                            details.id.clone(),
                            DiscoveredEvent {
                                event_id: details.id.clone(),
                                series_id: series_id.to_string(),
                                up_token,
                                down_token,
                                end_time,
                                price_to_beat: Some(price_to_beat),
                                title,
                            },
                        );
                    }

                    // Apply diff + emit events.
                    {
                        let mut series_events_guard = series_events.write().await;
                        let prev = series_events_guard
                            .entry(series_id.to_string())
                            .or_default();

                        let removed: Vec<String> = prev
                            .keys()
                            .filter(|id| !discovered.contains_key(*id))
                            .cloned()
                            .collect();
                        for event_id in removed {
                            prev.remove(&event_id);
                            manager.send_market_update(MarketUpdate::EventExpired { event_id });
                        }

                        for (event_id, ev) in discovered {
                            let should_send = match prev.get(&event_id) {
                                None => true,
                                Some(old) => {
                                    old.up_token != ev.up_token
                                        || old.down_token != ev.down_token
                                        || old.end_time != ev.end_time
                                        || old.price_to_beat != ev.price_to_beat
                                }
                            };

                            if should_send {
                                manager.send_market_update(MarketUpdate::EventDiscovered {
                                    event_id: ev.event_id.clone(),
                                    series_id: ev.series_id.clone(),
                                    up_token: ev.up_token.clone(),
                                    down_token: ev.down_token.clone(),
                                    end_time: ev.end_time,
                                    price_to_beat: ev.price_to_beat,
                                    title: ev.title.clone(),
                                });
                            }

                            prev.insert(event_id, ev);
                        }
                    }
                }

                // Reconcile token subscriptions to keep the set bounded.
                let desired = {
                    let guard = series_events.read().await;
                    let mut map: HashMap<String, crate::domain::Side> = HashMap::new();
                    for per_series in guard.values() {
                        for ev in per_series.values() {
                            map.insert(ev.up_token.clone(), crate::domain::Side::Up);
                            map.insert(ev.down_token.clone(), crate::domain::Side::Down);
                        }
                    }
                    map
                };

                let (added, removed, updated, total) = pm_ws.reconcile_token_sides(&desired).await;
                if (added + removed + updated) > 0 {
                    info!(
                        "Polymarket WS token reconcile: added={} removed={} updated={} total={}",
                        added, removed, updated, total
                    );
                    pm_ws.request_resubscribe();
                }
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

    #[test]
    fn test_feed_builder() {
        let builder = DataFeedBuilder::new()
            .with_symbols(vec!["BTCUSDT".into(), "ETHUSDT".into()])
            .with_series(vec!["10192".into()]);

        let binance = builder.build_binance();
        assert!(binance.is_some());
    }
}
