//! Backtest Data Collector
//!
//! Collects K-line and Polymarket price data for backtesting the Volatility Arbitrage strategy.
//!
//! ## Usage
//!
//! ```bash
//! # Collect data for backtesting
//! ploy collect-data --symbols BTC,ETH,SOL --duration 7d --output ./data/
//!
//! # Live collection (continuous)
//! ploy collect-data --live --symbols BTC,ETH,SOL --output ./data/
//! ```

mod csv_sink;
mod pm_collection;

use chrono::{DateTime, Duration, Timelike, Utc};
use rust_decimal::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};

use crate::adapters::PolymarketClient;
use crate::collector::BinanceKlineClient;
use crate::error::{PloyError, Result};

pub use csv_sink::{KlineCSV, PMPriceCSV};
pub use pm_collection::ActiveMarket;
use pm_collection::TrackedMarket;

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone)]
pub struct CollectorConfig {
    /// Symbols to collect (e.g., ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Output directory for CSV files
    pub output_dir: PathBuf,
    /// K-line collection interval in seconds (default: 900 = 15 min)
    pub kline_interval_secs: u64,
    /// PM price collection interval in seconds (default: 30)
    pub pm_interval_secs: u64,
    /// Whether to collect continuously or for a fixed duration
    pub continuous: bool,
    /// Duration to collect (if not continuous)
    pub duration_hours: Option<u64>,
    /// Legacy compatibility sink: write CSV artifacts.
    /// Default false because primary sink is DB.
    pub persist_csv: bool,
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
            output_dir: PathBuf::from("./data"),
            kline_interval_secs: 900, // 15 minutes
            pm_interval_secs: 30,     // 30 seconds
            continuous: true,
            duration_hours: None,
            persist_csv: false,
        }
    }
}

// ============================================================================
// Data Structures
// ============================================================================

// ============================================================================
// Backtest Data Collector
// ============================================================================

pub struct BacktestCollector {
    config: CollectorConfig,
    pm_client: Option<Arc<PolymarketClient>>,
    /// Tracked markets awaiting resolution
    tracked_markets: Arc<RwLock<HashMap<String, TrackedMarket>>>,
    /// Collection statistics
    stats: Arc<RwLock<CollectorStats>>,
}

#[derive(Debug, Clone, Default)]
pub struct CollectorStats {
    pub klines_collected: u64,
    pub pm_prices_collected: u64,
    pub markets_resolved: u64,
    pub start_time: Option<DateTime<Utc>>,
    pub last_kline_time: Option<DateTime<Utc>>,
    pub last_pm_time: Option<DateTime<Utc>>,
}

impl BacktestCollector {
    pub fn new(config: CollectorConfig) -> Self {
        Self {
            config,
            pm_client: None,
            tracked_markets: Arc::new(RwLock::new(HashMap::new())),
            stats: Arc::new(RwLock::new(CollectorStats::default())),
        }
    }

    pub fn with_pm_client(mut self, client: Arc<PolymarketClient>) -> Self {
        self.pm_client = Some(client);
        self
    }

    /// Start collecting data
    pub async fn run(&self) -> Result<()> {
        info!("Starting backtest data collector");
        info!("Symbols: {:?}", self.config.symbols);
        info!("Output directory: {:?}", self.config.output_dir);

        if self.config.persist_csv {
            self.init_csv_files()?;
        } else {
            info!("CSV sink disabled (persist_csv=false)");
        }

        // Set start time
        {
            let mut stats = self.stats.write().await;
            stats.start_time = Some(Utc::now());
        }

        // Calculate end time if not continuous
        let end_time = self
            .config
            .duration_hours
            .map(|h| Utc::now() + Duration::hours(h as i64));

        // Spawn collection tasks
        let kline_handle = self.spawn_kline_collector(end_time);
        let pm_handle = self.spawn_pm_collector(end_time);
        let resolution_handle = self.spawn_resolution_checker(end_time);

        // Wait for all tasks
        tokio::select! {
            r = kline_handle => {
                if let Err(e) = r {
                    error!("K-line collector error: {:?}", e);
                }
            }
            r = pm_handle => {
                if let Err(e) = r {
                    error!("PM collector error: {:?}", e);
                }
            }
            r = resolution_handle => {
                if let Err(e) = r {
                    error!("Resolution checker error: {:?}", e);
                }
            }
        }

        Ok(())
    }

    /// Spawn K-line collection task
    fn spawn_kline_collector(
        &self,
        end_time: Option<DateTime<Utc>>,
    ) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let client = BinanceKlineClient::new();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            // Wait until next 15-minute boundary
            let now = Utc::now();
            let next_boundary = Self::next_15min_boundary(now);
            let wait_secs = (next_boundary - now).num_seconds().max(0) as u64;

            if wait_secs > 0 {
                info!(
                    "Waiting {}s until next 15-min boundary for K-line collection",
                    wait_secs
                );
                sleep(std::time::Duration::from_secs(wait_secs)).await;
            }

            let mut ticker = interval(std::time::Duration::from_secs(config.kline_interval_secs));

            loop {
                ticker.tick().await;

                // Check if we should stop
                if let Some(end) = end_time {
                    if Utc::now() >= end {
                        info!("K-line collection duration reached, stopping");
                        break;
                    }
                }

                // Collect K-lines for each symbol
                for symbol in &config.symbols {
                    match client.fetch_klines(symbol, "15m", 1).await {
                        Ok(klines) => {
                            if let Some(kline) = klines.last() {
                                let write_ok = if config.persist_csv {
                                    match Self::append_kline(&config.output_dir, symbol, kline)
                                        .await
                                    {
                                        Ok(_) => true,
                                        Err(e) => {
                                            warn!("Failed to append K-line for {}: {}", symbol, e);
                                            false
                                        }
                                    }
                                } else {
                                    true
                                };

                                if write_ok {
                                    let mut s = stats.write().await;
                                    s.klines_collected += 1;
                                    s.last_kline_time = Some(Utc::now());
                                    debug!("Collected K-line for {} @ {}", symbol, kline.close);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch K-line for {}: {}", symbol, e);
                        }
                    }
                    // Small delay between requests
                    sleep(std::time::Duration::from_millis(200)).await;
                }

                info!("K-line collection cycle complete");
            }
        })
    }

    /// Get next 15-minute boundary time
    fn next_15min_boundary(from: DateTime<Utc>) -> DateTime<Utc> {
        let minute = from.minute();
        let next_quarter = ((minute / 15) + 1) * 15;

        if next_quarter >= 60 {
            from.date_naive()
                .and_hms_opt(from.hour(), 0, 0)
                .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc) + Duration::hours(1))
                .unwrap_or(from)
        } else {
            from.date_naive()
                .and_hms_opt(from.hour(), next_quarter, 0)
                .map(|dt| DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc))
                .unwrap_or(from)
        }
    }

    /// Get collection statistics
    pub async fn stats(&self) -> CollectorStats {
        self.stats.read().await.clone()
    }
}

// ============================================================================
// Standalone Collection Functions (for CLI)
// ============================================================================

/// Collect historical K-lines from Binance and save to CSV
pub async fn collect_historical_klines(
    symbols: &[String],
    output_path: &Path,
    days: u64,
) -> Result<u64> {
    let _client = BinanceKlineClient::new();
    let mut total_records = 0u64;

    // Create output file with header
    let mut file = File::create(output_path).map_err(|e| PloyError::Internal(e.to_string()))?;
    writeln!(
        file,
        "timestamp,datetime,symbol,open,high,low,close,volume,trades"
    )
    .map_err(|e| PloyError::Internal(e.to_string()))?;

    // Calculate how many 15-min candles we need
    let candles_per_day = 24 * 4; // 96 candles per day
    let total_candles = (days * candles_per_day) as usize;

    info!(
        "Collecting {} days of K-line data ({} candles per symbol)",
        days, total_candles
    );

    for symbol in symbols {
        info!("Fetching K-lines for {}...", symbol);

        // Binance limits to 1000 candles per request
        let mut collected = 0;
        let mut end_time = Utc::now().timestamp_millis();

        while collected < total_candles {
            let limit = (total_candles - collected).min(1000);

            let url = format!(
                "https://api.binance.com/api/v3/klines?symbol={}&interval=15m&limit={}&endTime={}",
                symbol, limit, end_time
            );

            let response = reqwest::get(&url)
                .await
                .map_err(|e| PloyError::Internal(e.to_string()))?;

            let data: Vec<Vec<serde_json::Value>> = response
                .json()
                .await
                .map_err(|e| PloyError::Internal(e.to_string()))?;

            if data.is_empty() {
                break;
            }

            // Write to CSV
            for row in &data {
                if row.len() < 11 {
                    continue;
                }

                let open_time = row[0].as_i64().unwrap_or(0);
                let datetime = DateTime::from_timestamp_millis(open_time)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                    .unwrap_or_default();

                writeln!(
                    file,
                    "{},{},{},{},{},{},{},{},{}",
                    open_time / 1000,
                    datetime,
                    symbol,
                    row[1].as_str().unwrap_or("0"),
                    row[2].as_str().unwrap_or("0"),
                    row[3].as_str().unwrap_or("0"),
                    row[4].as_str().unwrap_or("0"),
                    row[5].as_str().unwrap_or("0"),
                    row[8].as_u64().unwrap_or(0),
                )
                .map_err(|e| PloyError::Internal(e.to_string()))?;

                total_records += 1;
            }

            collected += data.len();

            // Get earliest timestamp for next request
            if let Some(first) = data.first() {
                end_time = first[0].as_i64().unwrap_or(0) - 1;
            }

            // Rate limiting
            sleep(std::time::Duration::from_millis(100)).await;
        }

        info!("Collected {} K-lines for {}", collected, symbol);
    }

    info!("Total K-line records collected: {}", total_records);
    Ok(total_records)
}

/// Print collection status
pub fn print_collector_status(stats: &CollectorStats) {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║              DATA COLLECTION STATUS                          ║");
    println!("╠══════════════════════════════════════════════════════════════╣");

    if let Some(start) = stats.start_time {
        let duration = Utc::now() - start;
        println!(
            "║ Running for: {:>10} minutes                             ║",
            duration.num_minutes()
        );
    }

    println!(
        "║ K-lines collected:     {:>10}                           ║",
        stats.klines_collected
    );
    println!(
        "║ PM prices collected:   {:>10}                           ║",
        stats.pm_prices_collected
    );
    println!(
        "║ Markets resolved:      {:>10}                           ║",
        stats.markets_resolved
    );

    if let Some(last) = stats.last_kline_time {
        println!(
            "║ Last K-line:           {}              ║",
            last.format("%H:%M:%S")
        );
    }
    if let Some(last) = stats.last_pm_time {
        println!(
            "║ Last PM price:         {}              ║",
            last.format("%H:%M:%S")
        );
    }

    println!("╚══════════════════════════════════════════════════════════════╝\n");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn test_next_15min_boundary() {
        let time = Utc.with_ymd_and_hms(2024, 1, 1, 10, 7, 30).unwrap();
        let next = BacktestCollector::next_15min_boundary(time);
        assert_eq!(next.minute(), 15);
        assert_eq!(next.hour(), 10);

        let time = Utc.with_ymd_and_hms(2024, 1, 1, 10, 47, 0).unwrap();
        let next = BacktestCollector::next_15min_boundary(time);
        assert_eq!(next.minute(), 0);
        assert_eq!(next.hour(), 11);
    }

    #[test]
    fn test_config_default() {
        let config = CollectorConfig::default();
        assert_eq!(config.symbols.len(), 3);
        assert_eq!(config.kline_interval_secs, 900);
        assert!(config.continuous);
    }
}
