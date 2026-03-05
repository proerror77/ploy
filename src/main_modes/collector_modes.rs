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

/// Crypto series IDs for "Up or Down" markets on Polymarket.
/// 5m: BTC=10684, ETH=10683, SOL=10686, XRP=10685
/// 15m: BTC=10192, ETH=10191, SOL=10423, XRP=10422
const CRYPTO_SERIES_IDS: &[&str] = &[
    "10684", "10683", "10686", "10685", // 5m
    "10192", "10191", "10423", "10422", // 15m
];

const PM_WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
const PM_REST_URL: &str = "https://clob.polymarket.com";

fn infer_symbol_from_slug(slug: &str) -> Option<&'static str> {
    let lower = slug.to_ascii_lowercase();
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
    info!("Collector sink: database tables (raw canonical). CSV sink is disabled in main collector path.");

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
    let collector = Arc::new(SyncCollector::new(collector_config).with_pool(store.pool().clone()));

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
async fn spawn_pm_price_bridge(collector: Arc<ploy::collector::SyncCollector>) {
    use ploy::adapters::{PolymarketClient, PolymarketWebSocket};
    use ploy::collector::{CollectorTokenTarget, PolymarketPrice};
    use ploy::domain::market::Side;

    // Create read-only PM client for event discovery
    let pm_client = match PolymarketClient::new(PM_REST_URL, true) {
        Ok(c) => c,
        Err(e) => {
            warn!(
                "Failed to create PM client for collector: {}. PM prices will be unavailable.",
                e
            );
            return;
        }
    };

    // Discover active tokens from crypto series.
    // token_id -> (slug, side) for mapping quotes back to markets.
    let mut token_to_market: HashMap<String, (String, Side)> = HashMap::new();
    let mut target_rows: Vec<CollectorTokenTarget> = Vec::new();
    // slug -> (yes_token_id, no_token_id) to persist event token ids with each sync record.
    let mut slug_token_map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut all_token_ids: Vec<String> = Vec::new();

    for series_id in CRYPTO_SERIES_IDS {
        match pm_client.get_all_active_events(series_id).await {
            Ok(events) => {
                let now = Utc::now();
                let soon_cutoff = now + chrono::Duration::hours(1);

                let mut candidates: Vec<_> = events.iter().collect();
                candidates.sort_by_key(|e| {
                    e.end_date
                        .as_deref()
                        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                        .map(|dt| dt.with_timezone(&Utc))
                        .unwrap_or(chrono::DateTime::<Utc>::MAX_UTC)
                });

                // Prefer windows ending soon; fall back to first few if parsing fails.
                let mut selected: Vec<_> = candidates
                    .into_iter()
                    .filter(|e| {
                        e.end_date
                            .as_deref()
                            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                            .map(|dt| dt.with_timezone(&Utc))
                            .is_some_and(|end| end <= soon_cutoff && end >= now - chrono::Duration::minutes(15))
                    })
                    .take(4)
                    .collect();

                if selected.is_empty() {
                    selected = events.iter().take(4).collect();
                }

                let mut discovered_this_series = 0usize;
                for event in selected {
                    let slug = event.slug.clone().unwrap_or_default();
                    if slug.is_empty() {
                        continue;
                    }

                    // The series endpoint omits nested market token IDs; fetch details and map to CLOB tokens.
                    let event_details = match pm_client.get_event_details(&event.id).await {
                        Ok(e) => e,
                        Err(e) => {
                            warn!("Failed to get event details for {}: {}", event.id, e);
                            continue;
                        }
                    };

                    for gamma_market in &event_details.markets {
                        let Some(condition_id) = gamma_market.condition_id.as_ref() else {
                            continue;
                        };

                        let mut clob_market = match pm_client.get_market(condition_id).await {
                            Ok(m) => m,
                            Err(e) => {
                                warn!("Failed to get CLOB market {}: {}", condition_id, e);
                                continue;
                            }
                        };

                        if clob_market.tokens.len() < 2 {
                            continue;
                        }

                        let is_first_up = {
                            let outcome = clob_market.tokens[0].outcome.to_lowercase();
                            outcome.contains("up") || outcome == "yes"
                        };
                        let mut tokens = clob_market.tokens.drain(..2);
                        let first = tokens.next().unwrap();
                        let second = tokens.next().unwrap();
                        let (up_token, down_token) = if is_first_up {
                            (first.token_id, second.token_id)
                        } else {
                            (second.token_id, first.token_id)
                        };

                        token_to_market.insert(up_token.clone(), (slug.clone(), Side::Up));
                        token_to_market.insert(down_token.clone(), (slug.clone(), Side::Down));

                        if let Some(symbol) = infer_symbol_from_slug(&slug) {
                            target_rows.push(
                                CollectorTokenTarget::new(up_token.clone(), "CRYPTO")
                                    .with_metadata(serde_json::json!({
                                        "symbol": symbol,
                                        "side": "UP",
                                        "slug": slug,
                                    })),
                            );
                            target_rows.push(
                                CollectorTokenTarget::new(down_token.clone(), "CRYPTO")
                                    .with_metadata(serde_json::json!({
                                        "symbol": symbol,
                                        "side": "DOWN",
                                        "slug": slug,
                                    })),
                            );
                        }

                        let side_tokens =
                            slug_token_map.entry(slug.clone()).or_insert((None, None));
                        side_tokens.0 = Some(up_token.clone());
                        side_tokens.1 = Some(down_token.clone());

                        all_token_ids.push(up_token);
                        all_token_ids.push(down_token);
                        discovered_this_series += 2;
                    }
                }
                debug!(
                    "Series {}: discovered {} tokens from {} events",
                    series_id,
                    discovered_this_series,
                    events.len()
                );
            }
            Err(e) => {
                warn!("Failed to discover events for series {}: {}", series_id, e);
            }
        }
    }

    if let Err(e) = collector.upsert_token_targets(&target_rows).await {
        debug!("collector target upsert failed: {}", e);
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
    let slug_token_map_for_bridge = slug_token_map.clone();
    tokio::spawn(async move {
        // slug -> (yes_price, no_price)
        let mut pm_state: HashMap<String, (Decimal, Decimal)> = HashMap::new();

        loop {
            match quote_rx.recv().await {
                Ok(update) => {
                    if let Some((slug, side)) = token_to_market.get(&update.token_id) {
                        let side_text = match side {
                            Side::Up => "UP",
                            Side::Down => "DOWN",
                        };
                        if let Err(e) = collector
                            .persist_polymarket_quote_tick(
                                &update.token_id,
                                side_text,
                                update.quote.best_bid,
                                update.quote.best_ask,
                                update.quote.bid_size,
                                update.quote.ask_size,
                                update.quote.timestamp,
                            )
                            .await
                        {
                            debug!("failed to persist polymarket quote tick: {}", e);
                        }

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
                        let (yes_token_id, no_token_id) = slug_token_map_for_bridge
                            .get(slug)
                            .cloned()
                            .unwrap_or((None, None));
                        collector
                            .update_polymarket_price(PolymarketPrice {
                                timestamp: update.quote.timestamp,
                                market_slug: slug.clone(),
                                yes_price: entry.0,
                                no_price: entry.1,
                                yes_token_id,
                                no_token_id,
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
