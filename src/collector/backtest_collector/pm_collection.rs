use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, sleep};
use tracing::{debug, info, warn};

use crate::adapters::PolymarketClient;
use crate::collector::BinanceKlineClient;
use crate::error::{PloyError, Result};

use super::{BacktestCollector, CollectorConfig, CollectorStats};

/// Tracked market for resolution monitoring
#[derive(Debug, Clone)]
pub(super) struct TrackedMarket {
    pub(super) market_id: String,
    pub(super) condition_id: String,
    pub(super) resolution_time: DateTime<Utc>,
}

/// Active market information
#[derive(Debug, Clone)]
pub struct ActiveMarket {
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub threshold: Decimal,
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub yes_bid: Decimal,
    pub yes_ask: Decimal,
    pub no_bid: Decimal,
    pub no_ask: Decimal,
    pub resolution_time: DateTime<Utc>,
}

impl BacktestCollector {
    /// Spawn PM price collection task
    pub(super) fn spawn_pm_collector(
        &self,
        end_time: Option<DateTime<Utc>>,
    ) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let pm_client = self.pm_client.clone();
        let stats = self.stats.clone();
        let tracked_markets = self.tracked_markets.clone();
        let kline_client = BinanceKlineClient::new();

        tokio::spawn(async move {
            run_pm_collector(
                config,
                pm_client,
                stats,
                tracked_markets,
                kline_client,
                end_time,
            )
            .await
        })
    }

    /// Spawn resolution checking task
    pub(super) fn spawn_resolution_checker(
        &self,
        end_time: Option<DateTime<Utc>>,
    ) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let pm_client = self.pm_client.clone();
        let stats = self.stats.clone();
        let tracked_markets = self.tracked_markets.clone();

        tokio::spawn(async move {
            run_resolution_checker(config, pm_client, stats, tracked_markets, end_time).await
        })
    }

    /// Fetch active 15-minute markets for a symbol
    pub(super) async fn fetch_active_markets(
        _client: &PolymarketClient,
        symbol: &str,
    ) -> Result<Vec<ActiveMarket>> {
        let coin = match symbol {
            "BTCUSDT" => "BTC",
            "ETHUSDT" => "ETH",
            "SOLUSDT" => "SOL",
            "XRPUSDT" => "XRP",
            _ => return Ok(Vec::new()),
        };

        let _search_term = format!("{} 15", coin);
        Ok(Vec::new())
    }

    /// Check if a market has resolved
    pub(super) async fn check_resolution(
        _client: &PolymarketClient,
        _condition_id: &str,
    ) -> Result<bool> {
        Err(PloyError::Internal("Not implemented".to_string()))
    }
}

async fn run_pm_collector(
    config: CollectorConfig,
    pm_client: Option<Arc<PolymarketClient>>,
    stats: Arc<RwLock<CollectorStats>>,
    tracked_markets: Arc<RwLock<HashMap<String, TrackedMarket>>>,
    kline_client: BinanceKlineClient,
    end_time: Option<DateTime<Utc>>,
) {
    let mut ticker = interval(std::time::Duration::from_secs(config.pm_interval_secs));

    loop {
        ticker.tick().await;

        if let Some(end) = end_time {
            if Utc::now() >= end {
                info!("PM collection duration reached, stopping");
                break;
            }
        }

        let Some(ref client) = pm_client else {
            debug!("No PM client configured, skipping PM collection");
            continue;
        };

        for symbol in &config.symbols {
            let spot_price = match kline_client.fetch_klines(symbol, "1m", 1).await {
                Ok(klines) => klines.last().map(|k| k.close).unwrap_or(Decimal::ZERO),
                Err(_) => Decimal::ZERO,
            };

            match BacktestCollector::fetch_active_markets(client, symbol).await {
                Ok(markets) => {
                    for market in markets {
                        let write_ok = if config.persist_csv {
                            match BacktestCollector::append_pm_price(
                                &config.output_dir,
                                &market,
                                spot_price,
                            )
                            .await
                            {
                                Ok(_) => true,
                                Err(e) => {
                                    warn!("Failed to append PM price: {}", e);
                                    false
                                }
                            }
                        } else {
                            true
                        };

                        if write_ok {
                            let mut s = stats.write().await;
                            s.pm_prices_collected += 1;
                            s.last_pm_time = Some(Utc::now());

                            let mut tracked = tracked_markets.write().await;
                            tracked.insert(
                                market.market_id.clone(),
                                TrackedMarket {
                                    market_id: market.market_id.clone(),
                                    condition_id: market.condition_id.clone(),
                                    resolution_time: market.resolution_time,
                                },
                            );
                        }
                    }
                }
                Err(e) => {
                    debug!("Failed to fetch markets for {}: {}", symbol, e);
                }
            }

            sleep(std::time::Duration::from_millis(200)).await;
        }
    }
}

async fn run_resolution_checker(
    config: CollectorConfig,
    pm_client: Option<Arc<PolymarketClient>>,
    stats: Arc<RwLock<CollectorStats>>,
    tracked_markets: Arc<RwLock<HashMap<String, TrackedMarket>>>,
    end_time: Option<DateTime<Utc>>,
) {
    let mut ticker = interval(std::time::Duration::from_secs(60));

    loop {
        ticker.tick().await;

        if let Some(end) = end_time {
            if Utc::now() >= end {
                info!("Resolution checker duration reached, stopping");
                break;
            }
        }

        let Some(ref client) = pm_client else {
            continue;
        };

        let now = Utc::now();
        let mut resolved = Vec::new();

        {
            let tracked = tracked_markets.read().await;
            for (_market_id, market) in tracked.iter() {
                if now > market.resolution_time + Duration::minutes(5) {
                    resolved.push(market.clone());
                }
            }
        }

        for market in resolved {
            if let Ok(outcome) =
                BacktestCollector::check_resolution(client, &market.condition_id).await
            {
                if let Err(e) = BacktestCollector::update_outcome(
                    &config.output_dir,
                    &market.market_id,
                    outcome,
                )
                .await
                {
                    warn!("Failed to update outcome for {}: {}", market.market_id, e);
                } else {
                    let mut s = stats.write().await;
                    s.markets_resolved += 1;
                    info!(
                        "Market {} resolved: {}",
                        market.market_id,
                        if outcome { "YES" } else { "NO" }
                    );
                }

                let mut tracked = tracked_markets.write().await;
                tracked.remove(&market.market_id);
            }
        }
    }
}
