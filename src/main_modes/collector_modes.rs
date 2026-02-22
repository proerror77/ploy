use chrono::Utc;
use ploy::adapters::PostgresStore;
use ploy::config::AppConfig;
use ploy::error::{PloyError, Result};
use tokio::signal;
use tokio::time::Duration;
use tracing::{error, info, warn};

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

    // Create collector with database
    let collector = SyncCollector::new(collector_config).with_pool(store.pool().clone());

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

        let inserted = collector.backfill_asset(asset_id, start_ms, end_ms).await?;
        info!(
            asset_id = asset_id.as_str(),
            inserted, "orderbook-history backfill done"
        );
    }

    Ok(())
}
