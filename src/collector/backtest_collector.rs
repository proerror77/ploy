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

use chrono::{DateTime, Utc, Duration, Timelike};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{Write, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, sleep};
use tracing::{debug, info, warn, error};

use crate::collector::BinanceKlineClient;
use crate::error::{PloyError, Result};
use crate::adapters::PolymarketClient;

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
}

impl Default for CollectorConfig {
    fn default() -> Self {
        Self {
            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
            ],
            output_dir: PathBuf::from("./data"),
            kline_interval_secs: 900, // 15 minutes
            pm_interval_secs: 30,     // 30 seconds
            continuous: true,
            duration_hours: None,
        }
    }
}

// ============================================================================
// Data Structures
// ============================================================================

/// K-line record for CSV export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlineCSV {
    pub timestamp: i64,
    pub datetime: String,
    pub symbol: String,
    pub open: String,
    pub high: String,
    pub low: String,
    pub close: String,
    pub volume: String,
    pub trades: u64,
}

/// PM price record for CSV export
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PMPriceCSV {
    pub timestamp: i64,
    pub datetime: String,
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub threshold: String,
    pub spot_price: String,
    pub yes_price: String,
    pub no_price: String,
    pub yes_bid: String,
    pub yes_ask: String,
    pub no_bid: String,
    pub no_ask: String,
    pub resolution_time: i64,
    pub time_remaining_secs: i64,
    pub outcome: String, // "pending", "YES", "NO"
}

/// Tracked market for resolution monitoring
#[derive(Debug, Clone)]
struct TrackedMarket {
    market_id: String,
    condition_id: String,
    symbol: String,
    threshold: Decimal,
    resolution_time: DateTime<Utc>,
    recorded_at: DateTime<Utc>,
}

// ============================================================================
// Backtest Data Collector
// ============================================================================

pub struct BacktestCollector {
    config: CollectorConfig,
    kline_client: BinanceKlineClient,
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
            kline_client: BinanceKlineClient::new(),
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

        // Create output directory
        create_dir_all(&self.config.output_dir)
            .map_err(|e| PloyError::Internal(format!("Failed to create output dir: {}", e)))?;

        // Initialize CSV files with headers
        self.init_csv_files()?;

        // Set start time
        {
            let mut stats = self.stats.write().await;
            stats.start_time = Some(Utc::now());
        }

        // Calculate end time if not continuous
        let end_time = self.config.duration_hours.map(|h| {
            Utc::now() + Duration::hours(h as i64)
        });

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

    /// Initialize CSV files with headers
    fn init_csv_files(&self) -> Result<()> {
        // K-line CSV
        let kline_path = self.config.output_dir.join("klines.csv");
        if !kline_path.exists() {
            let mut file = File::create(&kline_path)
                .map_err(|e| PloyError::Internal(e.to_string()))?;
            writeln!(file, "timestamp,datetime,symbol,open,high,low,close,volume,trades")
                .map_err(|e| PloyError::Internal(e.to_string()))?;
            info!("Created {}", kline_path.display());
        }

        // PM price CSV
        let pm_path = self.config.output_dir.join("pm_prices.csv");
        if !pm_path.exists() {
            let mut file = File::create(&pm_path)
                .map_err(|e| PloyError::Internal(e.to_string()))?;
            writeln!(file, "timestamp,datetime,market_id,condition_id,symbol,threshold,spot_price,yes_price,no_price,yes_bid,yes_ask,no_bid,no_ask,resolution_time,time_remaining_secs,outcome")
                .map_err(|e| PloyError::Internal(e.to_string()))?;
            info!("Created {}", pm_path.display());
        }

        Ok(())
    }

    /// Spawn K-line collection task
    fn spawn_kline_collector(&self, end_time: Option<DateTime<Utc>>) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let client = BinanceKlineClient::new();
        let stats = self.stats.clone();

        tokio::spawn(async move {
            // Wait until next 15-minute boundary
            let now = Utc::now();
            let next_boundary = Self::next_15min_boundary(now);
            let wait_secs = (next_boundary - now).num_seconds().max(0) as u64;

            if wait_secs > 0 {
                info!("Waiting {}s until next 15-min boundary for K-line collection", wait_secs);
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
                                if let Err(e) = Self::append_kline(&config.output_dir, symbol, kline).await {
                                    warn!("Failed to append K-line for {}: {}", symbol, e);
                                } else {
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

    /// Spawn PM price collection task
    fn spawn_pm_collector(&self, end_time: Option<DateTime<Utc>>) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let pm_client = self.pm_client.clone();
        let stats = self.stats.clone();
        let tracked_markets = self.tracked_markets.clone();
        let kline_client = BinanceKlineClient::new();

        tokio::spawn(async move {
            let mut ticker = interval(std::time::Duration::from_secs(config.pm_interval_secs));

            loop {
                ticker.tick().await;

                // Check if we should stop
                if let Some(end) = end_time {
                    if Utc::now() >= end {
                        info!("PM collection duration reached, stopping");
                        break;
                    }
                }

                // Skip if no PM client
                let Some(ref client) = pm_client else {
                    debug!("No PM client configured, skipping PM collection");
                    continue;
                };

                // Collect PM prices for each symbol
                for symbol in &config.symbols {
                    // Get current spot price from Binance
                    let spot_price = match kline_client.fetch_klines(symbol, "1m", 1).await {
                        Ok(klines) => klines.last().map(|k| k.close).unwrap_or(Decimal::ZERO),
                        Err(_) => Decimal::ZERO,
                    };

                    // Fetch active 15-minute markets for this symbol
                    match Self::fetch_active_markets(client, symbol).await {
                        Ok(markets) => {
                            for market in markets {
                                if let Err(e) = Self::append_pm_price(
                                    &config.output_dir,
                                    &market,
                                    spot_price,
                                ).await {
                                    warn!("Failed to append PM price: {}", e);
                                } else {
                                    let mut s = stats.write().await;
                                    s.pm_prices_collected += 1;
                                    s.last_pm_time = Some(Utc::now());

                                    // Track for resolution
                                    let mut tracked = tracked_markets.write().await;
                                    tracked.insert(market.market_id.clone(), TrackedMarket {
                                        market_id: market.market_id.clone(),
                                        condition_id: market.condition_id.clone(),
                                        symbol: symbol.clone(),
                                        threshold: market.threshold,
                                        resolution_time: market.resolution_time,
                                        recorded_at: Utc::now(),
                                    });
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
        })
    }

    /// Spawn resolution checking task
    fn spawn_resolution_checker(&self, end_time: Option<DateTime<Utc>>) -> tokio::task::JoinHandle<()> {
        let config = self.config.clone();
        let pm_client = self.pm_client.clone();
        let stats = self.stats.clone();
        let tracked_markets = self.tracked_markets.clone();

        tokio::spawn(async move {
            let mut ticker = interval(std::time::Duration::from_secs(60)); // Check every minute

            loop {
                ticker.tick().await;

                // Check if we should stop
                if let Some(end) = end_time {
                    if Utc::now() >= end {
                        info!("Resolution checker duration reached, stopping");
                        break;
                    }
                }

                let Some(ref client) = pm_client else {
                    continue;
                };

                // Check tracked markets for resolution
                let now = Utc::now();
                let mut resolved = Vec::new();

                {
                    let tracked = tracked_markets.read().await;
                    for (market_id, market) in tracked.iter() {
                        // If past resolution time, check outcome
                        if now > market.resolution_time + Duration::minutes(5) {
                            resolved.push(market.clone());
                        }
                    }
                }

                // Update resolved markets
                for market in resolved {
                    if let Ok(outcome) = Self::check_resolution(client, &market.condition_id).await {
                        // Update the CSV with outcome
                        if let Err(e) = Self::update_outcome(&config.output_dir, &market.market_id, outcome).await {
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

                        // Remove from tracked
                        let mut tracked = tracked_markets.write().await;
                        tracked.remove(&market.market_id);
                    }
                }
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

    /// Append a K-line record to CSV
    async fn append_kline(output_dir: &Path, symbol: &str, kline: &super::binance_klines::Kline) -> Result<()> {
        let path = output_dir.join("klines.csv");
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| PloyError::Internal(e.to_string()))?;

        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{}",
            kline.open_time.timestamp(),
            kline.open_time.format("%Y-%m-%d %H:%M:%S"),
            symbol,
            kline.open,
            kline.high,
            kline.low,
            kline.close,
            kline.volume,
            kline.trades,
        ).map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Append a PM price record to CSV
    async fn append_pm_price(output_dir: &Path, market: &ActiveMarket, spot_price: Decimal) -> Result<()> {
        let path = output_dir.join("pm_prices.csv");
        let file = OpenOptions::new()
            .append(true)
            .open(&path)
            .map_err(|e| PloyError::Internal(e.to_string()))?;

        let now = Utc::now();
        let time_remaining = (market.resolution_time - now).num_seconds().max(0);

        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            now.timestamp(),
            now.format("%Y-%m-%d %H:%M:%S"),
            market.market_id,
            market.condition_id,
            market.symbol,
            market.threshold,
            spot_price,
            market.yes_price,
            market.no_price,
            market.yes_bid,
            market.yes_ask,
            market.no_bid,
            market.no_ask,
            market.resolution_time.timestamp(),
            time_remaining,
            "pending",
        ).map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Fetch active 15-minute markets for a symbol
    async fn fetch_active_markets(client: &PolymarketClient, symbol: &str) -> Result<Vec<ActiveMarket>> {
        // Map symbol to coin name
        let coin = match symbol {
            "BTCUSDT" => "BTC",
            "ETHUSDT" => "ETH",
            "SOLUSDT" => "SOL",
            "XRPUSDT" => "XRP",
            _ => return Ok(Vec::new()),
        };

        // Search for active 15-minute markets
        // This is a simplified version - in production, you'd use the actual PM API
        let search_term = format!("{} 15", coin);

        // Use client to search markets
        // For now, return empty - this needs to be connected to actual PM API
        // In the real implementation, this would call client.search_markets()

        Ok(Vec::new())
    }

    /// Check if a market has resolved
    async fn check_resolution(client: &PolymarketClient, condition_id: &str) -> Result<bool> {
        // Check market resolution status
        // This needs to be connected to actual PM API
        // Returns true for YES, false for NO
        Err(PloyError::Internal("Not implemented".to_string()))
    }

    /// Update outcome in CSV file
    async fn update_outcome(output_dir: &Path, market_id: &str, outcome: bool) -> Result<()> {
        // This is a simplified version - in production you'd update the CSV properly
        // For now, we'll append a new line indicating resolution
        let path = output_dir.join("resolutions.csv");

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| PloyError::Internal(e.to_string()))?;

        let mut writer = BufWriter::new(file);
        writeln!(
            writer,
            "{},{},{}",
            Utc::now().timestamp(),
            market_id,
            if outcome { "YES" } else { "NO" },
        ).map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Get collection statistics
    pub async fn stats(&self) -> CollectorStats {
        self.stats.read().await.clone()
    }
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

// ============================================================================
// Standalone Collection Functions (for CLI)
// ============================================================================

/// Collect historical K-lines from Binance and save to CSV
pub async fn collect_historical_klines(
    symbols: &[String],
    output_path: &Path,
    days: u64,
) -> Result<u64> {
    let client = BinanceKlineClient::new();
    let mut total_records = 0u64;

    // Create output file with header
    let mut file = File::create(output_path)
        .map_err(|e| PloyError::Internal(e.to_string()))?;
    writeln!(file, "timestamp,datetime,symbol,open,high,low,close,volume,trades")
        .map_err(|e| PloyError::Internal(e.to_string()))?;

    // Calculate how many 15-min candles we need
    let candles_per_day = 24 * 4; // 96 candles per day
    let total_candles = (days * candles_per_day) as usize;

    info!("Collecting {} days of K-line data ({} candles per symbol)", days, total_candles);

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

            let response = reqwest::get(&url).await
                .map_err(|e| PloyError::Internal(e.to_string()))?;

            let data: Vec<Vec<serde_json::Value>> = response.json().await
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
                ).map_err(|e| PloyError::Internal(e.to_string()))?;

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
        println!("║ Running for: {:>10} minutes                             ║", duration.num_minutes());
    }

    println!("║ K-lines collected:     {:>10}                           ║", stats.klines_collected);
    println!("║ PM prices collected:   {:>10}                           ║", stats.pm_prices_collected);
    println!("║ Markets resolved:      {:>10}                           ║", stats.markets_resolved);

    if let Some(last) = stats.last_kline_time {
        println!("║ Last K-line:           {}              ║", last.format("%H:%M:%S"));
    }
    if let Some(last) = stats.last_pm_time {
        println!("║ Last PM price:         {}              ║", last.format("%H:%M:%S"));
    }

    println!("╚══════════════════════════════════════════════════════════════╝\n");
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

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
