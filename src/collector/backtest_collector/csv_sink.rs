use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions, create_dir_all};
use std::io::{BufWriter, Write};
use std::path::Path;
use tracing::info;

use crate::error::{PloyError, Result};

use super::{ActiveMarket, BacktestCollector};

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
    pub outcome: String,
}

impl BacktestCollector {
    /// Initialize CSV files with headers
    pub(super) fn init_csv_files(&self) -> Result<()> {
        create_dir_all(&self.config.output_dir)
            .map_err(|e| PloyError::Internal(format!("Failed to create output dir: {}", e)))?;

        let kline_path = self.config.output_dir.join("klines.csv");
        if !kline_path.exists() {
            let mut file =
                File::create(&kline_path).map_err(|e| PloyError::Internal(e.to_string()))?;
            writeln!(
                file,
                "timestamp,datetime,symbol,open,high,low,close,volume,trades"
            )
            .map_err(|e| PloyError::Internal(e.to_string()))?;
            info!("Created {}", kline_path.display());
        }

        let pm_path = self.config.output_dir.join("pm_prices.csv");
        if !pm_path.exists() {
            let mut file =
                File::create(&pm_path).map_err(|e| PloyError::Internal(e.to_string()))?;
            writeln!(file, "timestamp,datetime,market_id,condition_id,symbol,threshold,spot_price,yes_price,no_price,yes_bid,yes_ask,no_bid,no_ask,resolution_time,time_remaining_secs,outcome")
                .map_err(|e| PloyError::Internal(e.to_string()))?;
            info!("Created {}", pm_path.display());
        }

        Ok(())
    }

    /// Append a K-line record to CSV
    pub(super) async fn append_kline(
        output_dir: &Path,
        symbol: &str,
        kline: &super::super::binance_klines::Kline,
    ) -> Result<()> {
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
        )
        .map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Append a PM price record to CSV
    pub(super) async fn append_pm_price(
        output_dir: &Path,
        market: &ActiveMarket,
        spot_price: rust_decimal::Decimal,
    ) -> Result<()> {
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
        )
        .map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }

    /// Update outcome in CSV file
    pub(super) async fn update_outcome(
        output_dir: &Path,
        market_id: &str,
        outcome: bool,
    ) -> Result<()> {
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
        )
        .map_err(|e| PloyError::Internal(e.to_string()))?;

        Ok(())
    }
}
