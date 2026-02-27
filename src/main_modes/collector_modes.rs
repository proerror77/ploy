use chrono::Utc;
use ploy::adapters::PostgresStore;
use ploy::config::AppConfig;
use ploy::error::{PloyError, Result};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::signal;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

/// Crypto series IDs for "Up or Down" 15-min markets on Polymarket.
/// BTC: 10192, ETH: 10191, SOL: 10423 + 10422
const CRYPTO_SERIES_IDS: &[&str] = &["10192", "10191", "10423", "10422"];

const PM_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const PM_REST_URL: &str = "https://clob.polymarket.com";

pub async fn run_collect_mode(symbols: &str, markets: Option<&str>, duration: u64) -> Result<()> {
    use ploy::collector::{SyncCollector, SyncCollectorConfig};

    info!("Starting data collector...");

    // Parse symbols
    let binance_symbols: Vec<String> = symbols
        .split(',')
        .map(|s| s.trim().to_uppercase())
        .collect();

    // Parse Polymarket markets
    let polymarket_slugs: Vec<String> = markets
        .map(|m| m.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();

    info!("Binance symbols: {:?}", binance_symbols);
    info!("Polymarket markets: {:?}", polymarket_slugs);

    // Load config for database URL
    let config = AppConfig::load()?;

    // Create collector config
    let collector_config = SyncCollectorConfig {
        binance_symbols: binance_symbols.clone(),
        polymarket_slugs,
        snapshot_interval_ms: 100,
        database_url: config.database.url.clone(),
    };

    // Create database pool
    let store = PostgresStore::new(&config.database.url, 5).await?;

    // Create collector with database, wrapped in Arc for shared access
    let collector = Arc::new(
        SyncCollector::new(collector_config).with_pool(store.pool().clone()),
    );

    // Subscribe to updates for logging
    let mut rx = collector.subscribe();

    // Spawn update logger
    tokio::spawn(async move {
        let mut count = 0u64;
        loop {
            match rx.recv().await {
                Ok(record) => {
                    count += 1;
                    if count % 100 == 0 {
                        info!(
                            "[{}] {} mid={:.2} obi5={:.4} pm_yes={:?}",
                            count,
                            record.symbol,
                            record.bn_mid_price,
                            record.bn_obi_5,
                            record.pm_yes_price
                        );
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Logger lagged {} messages", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    break;
                }
            }
        }
    });

    // === Polymarket WebSocket integration ===
    // Discover active PM tokens and subscribe for real-time price data
    spawn_pm_price_bridge(Arc::clone(&collector)).await;

    // Run collector
    if duration > 0 {
        info!("Collecting for {} minutes...", duration);
        tokio::select! {
            result = collector.run() => {
                if let Err(e) = result {
                    error!("Collector error: {}", e);
                }
            }
            _ = tokio::time::sleep(Duration::from_secs(duration * 60)) => {
                info!("Collection duration reached, stopping...");
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, stopping...");
            }
        }
    } else {
        info!("Collecting indefinitely (Ctrl+C to stop)...");
        tokio::select! {
            result = collector.run() => {
                if let Err(e) = result {
                    error!("Collector error: {}", e);
                }
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, stopping...");
            }
        }
    }

    info!("Data collection stopped");
    Ok(())
}

/// Discover active PM tokens for crypto series and spawn a WebSocket bridge
/// that feeds real-time PM prices into the collector.
async fn spawn_pm_price_bridge(
    collector: Arc<ploy::collector::SyncCollector>,
) {
    use ploy::adapters::{PolymarketClient, PolymarketWebSocket};
    use ploy::collector::PolymarketPrice;
    use ploy::domain::market::Side;

    // Create read-only PM client for event discovery
    let pm_client = match PolymarketClient::new(PM_REST_URL, true) {
        Ok(c) => c,
        Err(e) => {
            warn!("Failed to create PM client for collector: {}. PM prices will be unavailable.", e);
            return;
        }
    };

    // Discover active tokens from crypto series
    // token_id -> (slug, side) for mapping quotes back to markets
    let mut token_to_market: HashMap<String, (String, Side)> = HashMap::new();
    let mut all_token_ids: Vec<String> = Vec::new();

    for series_id in CRYPTO_SERIES_IDS {
        match pm_client.get_all_active_events(series_id).await {
            Ok(events) => {
                // Take events with end_date in the next hour (active windows)
                for event in events.iter().take(4) {
                    let slug = event.slug.clone().unwrap_or_default();
                    for market in &event.markets {
                        if let Some(tokens) = &market.tokens {
                            for token in tokens {
                                let side = if token.outcome.to_lowercase().contains("up")
                                    || token.outcome.to_lowercase() == "yes"
                                {
                                    Side::Up
                                } else {
                                    Side::Down
                                };
                                token_to_market.insert(
                                    token.token_id.clone(),
                                    (slug.clone(), side),
                                );
                                all_token_ids.push(token.token_id.clone());
                            }
                        }
                    }
                }
                debug!(
                    "Series {}: discovered {} tokens from {} events",
                    series_id,
                    all_token_ids.len(),
                    events.len()
                );
            }
            Err(e) => {
                warn!("Failed to discover events for series {}: {}", series_id, e);
            }
        }
    }

    if all_token_ids.is_empty() {
        warn!("No PM tokens discovered. PM prices will be unavailable.");
        return;
    }

    info!(
        "Collector PM bridge: discovered {} tokens across {} markets",
        all_token_ids.len(),
        token_to_market.len() / 2 // UP+DOWN = 1 market
    );

    // Create PM WebSocket and subscribe
    let pm_ws = Arc::new(PolymarketWebSocket::new(PM_WS_URL));
    let mut quote_rx = pm_ws.subscribe_updates();

    // Register token sides for correct quote mapping
    for (token_id, (_slug, side)) in &token_to_market {
        pm_ws.register_token(token_id, *side).await;
    }

    // Spawn PM WebSocket runner
    let ws_tokens = all_token_ids.clone();
    let ws = Arc::clone(&pm_ws);
    tokio::spawn(async move {
        if let Err(e) = ws.run(ws_tokens).await {
            error!("Collector PM WebSocket error: {}", e);
        }
    });

    // Spawn quote bridge: QuoteUpdate -> PolymarketPrice -> collector
    // Maintains latest (yes, no) per slug and pushes full updates
    tokio::spawn(async move {
        // slug -> (yes_price, no_price)
        let mut pm_state: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        loop {
            match quote_rx.recv().await {
                Ok(update) => {
                    if let Some((slug, side)) = token_to_market.get(&update.token_id) {
                        let entry = pm_state
                            .entry(slug.clone())
                            .or_insert((Decimal::ZERO, Decimal::ZERO));

                        // Update the relevant side's price (use best_ask as "price")
                        let price = update
                            .quote
                            .best_ask
                            .or(update.quote.best_bid)
                            .unwrap_or(Decimal::ZERO);

                        match side {
                            Side::Up => entry.0 = price,
                            Side::Down => entry.1 = price,
                        }

                        // Push full update to collector
                        collector
                            .update_polymarket_price(PolymarketPrice {
                                timestamp: update.quote.timestamp,
                                market_slug: slug.clone(),
                                yes_price: entry.0,
                                no_price: entry.1,
                            })
                            .await;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    debug!("PM bridge lagged {} messages", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    info!("PM bridge channel closed");
                    break;
                }
            }
        }
    });
}

pub async fn run_orderbook_history_mode(
    config_path: &str,
    asset_ids: &str,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    lookback_secs: u64,
    levels: usize,
    sample_ms: i64,
    limit: usize,
    max_pages: usize,
    base_url: &str,
    resume_from_db: bool,
) -> Result<()> {
    use ploy::collector::{OrderbookHistoryCollector, OrderbookHistoryCollectorConfig};

    let ids: Vec<String> = asset_ids
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if ids.is_empty() {
        return Err(PloyError::Validation(
            "--asset-ids must contain at least one token id".to_string(),
        ));
    }

    // Load config for database URL.
    let cfg = AppConfig::load_from(config_path)?;
    let store = PostgresStore::new(&cfg.database.url, 5).await?;

    let mut col_cfg = OrderbookHistoryCollectorConfig::default();
    col_cfg.clob_base_url = base_url.trim_end_matches('/').to_string();
    col_cfg.levels = levels;
    col_cfg.sample_ms = sample_ms;
    col_cfg.page_limit = limit;
    col_cfg.max_pages = max_pages;

    let collector = OrderbookHistoryCollector::new(store.pool().clone(), col_cfg);
    collector.ensure_tables().await?;

    let now_ms: i64 = Utc::now().timestamp_millis();
    let end_ms = end_ms.unwrap_or(now_ms);

    for asset_id in &ids {
        let fallback_start_ms =
            start_ms.unwrap_or_else(|| end_ms.saturating_sub(lookback_secs as i64 * 1000));
        let start_ms = if resume_from_db {
            let last_ms = collector.last_ts_ms_for_asset(asset_id).await?;
            let resumed_ms = last_ms.saturating_add(1);

            // Safety: if there is no history for this asset yet, or the resume point is
            // far in the past, clamp to a sane lookback window instead of requesting
            // from the unix epoch (which can trigger huge backfills / rate limiting).
            if last_ms <= 0 || resumed_ms < fallback_start_ms {
                fallback_start_ms
            } else {
                resumed_ms
            }
        } else {
            fallback_start_ms
        };

        info!(
            asset_id = asset_id.as_str(),
            start_ms,
            end_ms,
            levels,
            sample_ms,
            limit,
            max_pages,
            "starting orderbook-history backfill"
        );

        let condition_id_override = match sqlx::query_scalar::<_, String>(
            r#"
            SELECT NULLIF(BTRIM(metadata->>'condition_id'), '')
            FROM collector_token_targets
            WHERE token_id = $1
            "#,
        )
        .bind(asset_id)
        .fetch_optional(store.pool())
        .await
        {
            Ok(v) => v,
            Err(_) => None,
        };

        let inserted = collector
            .backfill_asset_with_condition(
                asset_id,
                condition_id_override.as_deref(),
                start_ms,
                end_ms,
            )
            .await?;
        info!(
            asset_id = asset_id.as_str(),
            inserted, "orderbook-history backfill done"
        );
    }

    Ok(())
}
