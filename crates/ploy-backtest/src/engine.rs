//! Backtest and Paper Trading Framework
//!
//! This module provides:
//! 1. Historical data loading from CSV/JSON files
//! 2. Core backtest types (snapshots, trades, results)
//! 3. Volatility calculation utilities
//! 4. Report generation for backtest results

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use tracing::{info, warn};

// ============================================================================
// Historical Data Structures
// ============================================================================

/// Historical K-line (candlestick) data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KlineRecord {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
}

/// Historical Polymarket price snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PMPriceRecord {
    pub timestamp: DateTime<Utc>,
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub threshold_price: Decimal,
    pub yes_price: Decimal,
    pub no_price: Decimal,
    pub yes_bid: Decimal,
    pub yes_ask: Decimal,
    pub resolution_time: DateTime<Utc>,
    pub outcome: Option<bool>, // true = YES won, false = NO won
}

/// Combined snapshot for backtesting
#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub spot_price: Decimal,
    pub threshold_price: Decimal,
    pub yes_price: Decimal,
    pub yes_ask: Decimal,
    pub time_remaining_secs: u64,
    pub resolution_time: DateTime<Utc>,
    pub market_id: String,
    pub condition_id: String,
    pub kline_volatility: f64,
    pub tick_volatility: Option<f64>,
    pub outcome: Option<bool>,
}

// ============================================================================
// Backtest Results
// ============================================================================

/// Individual backtest trade result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub entry_time: DateTime<Utc>,
    pub exit_time: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub direction: String, // "YES" or "NO"
    pub entry_price: Decimal,
    pub exit_price: Decimal,
    pub shares: u64,
    pub pnl: Decimal,
    pub pnl_pct: Decimal,
    pub won: bool,
    // Signal details
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
}

/// Backtest summary statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResults {
    pub start_time: DateTime<Utc>,
    pub end_time: DateTime<Utc>,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub avg_pnl_per_trade: Decimal,
    pub max_drawdown: Decimal,
    pub sharpe_ratio: f64,
    pub profit_factor: f64,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub largest_win: Decimal,
    pub largest_loss: Decimal,
    pub avg_holding_time_secs: f64,
    pub trades_by_symbol: HashMap<String, SymbolStats>,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<(DateTime<Utc>, Decimal)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub win_rate: f64,
    pub total_pnl: Decimal,
}

impl Default for BacktestResults {
    fn default() -> Self {
        Self {
            start_time: Utc::now(),
            end_time: Utc::now(),
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            total_pnl: Decimal::ZERO,
            total_volume: Decimal::ZERO,
            avg_pnl_per_trade: Decimal::ZERO,
            max_drawdown: Decimal::ZERO,
            sharpe_ratio: 0.0,
            profit_factor: 0.0,
            avg_win: Decimal::ZERO,
            avg_loss: Decimal::ZERO,
            largest_win: Decimal::ZERO,
            largest_loss: Decimal::ZERO,
            avg_holding_time_secs: 0.0,
            trades_by_symbol: HashMap::new(),
            trades: Vec::new(),
            equity_curve: Vec::new(),
        }
    }
}

// PLACEHOLDER_DATA_LOADING

// ============================================================================
// Data Loading
// ============================================================================

/// Load K-line data from CSV file
/// Expected format: timestamp,symbol,open,high,low,close,volume
pub fn load_klines_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<KlineRecord>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        if i == 0 {
            continue; // Skip header
        }

        let line = line.map_err(|e| format!("Failed to read line {}: {}", i, e))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 7 {
            warn!("Skipping malformed line {}: insufficient columns", i);
            continue;
        }

        let timestamp =
            parse_timestamp(parts[0]).ok_or_else(|| format!("Invalid timestamp at line {}", i))?;

        let record = KlineRecord {
            timestamp,
            symbol: parts[1].to_string(),
            open: Decimal::from_str(parts[2]).unwrap_or(Decimal::ZERO),
            high: Decimal::from_str(parts[3]).unwrap_or(Decimal::ZERO),
            low: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            close: Decimal::from_str(parts[5]).unwrap_or(Decimal::ZERO),
            volume: Decimal::from_str(parts[6]).unwrap_or(Decimal::ZERO),
        };

        records.push(record);
    }

    info!("Loaded {} K-line records", records.len());
    Ok(records)
}

/// Load PM price data from CSV file
/// Expected format: timestamp,market_id,condition_id,symbol,threshold,yes_price,no_price,yes_bid,yes_ask,resolution_time,outcome
pub fn load_pm_prices_from_csv<P: AsRef<Path>>(path: P) -> Result<Vec<PMPriceRecord>, String> {
    let file = File::open(path).map_err(|e| format!("Failed to open file: {}", e))?;
    let reader = BufReader::new(file);
    let mut records = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        if i == 0 {
            continue; // Skip header
        }

        let line = line.map_err(|e| format!("Failed to read line {}: {}", i, e))?;
        let parts: Vec<&str> = line.split(',').collect();

        if parts.len() < 11 {
            warn!("Skipping malformed line {}: insufficient columns", i);
            continue;
        }

        let timestamp =
            parse_timestamp(parts[0]).ok_or_else(|| format!("Invalid timestamp at line {}", i))?;
        let resolution_time = parse_timestamp(parts[9])
            .ok_or_else(|| format!("Invalid resolution_time at line {}", i))?;

        let outcome = match parts[10].trim().to_lowercase().as_str() {
            "yes" | "true" | "1" => Some(true),
            "no" | "false" | "0" => Some(false),
            _ => None,
        };

        let record = PMPriceRecord {
            timestamp,
            market_id: parts[1].to_string(),
            condition_id: parts[2].to_string(),
            symbol: parts[3].to_string(),
            threshold_price: Decimal::from_str(parts[4]).unwrap_or(Decimal::ZERO),
            yes_price: Decimal::from_str(parts[5]).unwrap_or(dec!(0.5)),
            no_price: Decimal::from_str(parts[6]).unwrap_or(dec!(0.5)),
            yes_bid: Decimal::from_str(parts[7]).unwrap_or(dec!(0.5)),
            yes_ask: Decimal::from_str(parts[8]).unwrap_or(dec!(0.5)),
            resolution_time,
            outcome,
        };

        records.push(record);
    }

    info!("Loaded {} PM price records", records.len());
    Ok(records)
}

// PLACEHOLDER_PARSE_TIMESTAMP

pub(crate) fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    // Try various formats
    if let Ok(ts) = s.parse::<i64>() {
        // Unix timestamp (seconds or milliseconds)
        if ts > 1_000_000_000_000 {
            return Utc.timestamp_millis_opt(ts).single();
        } else {
            return Utc.timestamp_opt(ts, 0).single();
        }
    }

    // Try ISO 8601 format
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }

    // Try common datetime formats
    let formats = [
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
    ];

    for fmt in &formats {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&dt));
        }
    }

    None
}

// ============================================================================
// Volatility Calculation
// ============================================================================

/// Calculate historical volatility from K-lines
/// Returns 15-minute volatility as percentage (e.g., 0.003 = 0.3%)
pub fn calculate_kline_volatility(klines: &[KlineRecord], lookback: usize) -> f64 {
    if klines.len() < 2 {
        return 0.003; // Default 0.3%
    }

    let n = klines.len().min(lookback);
    let recent = &klines[klines.len() - n..];

    // Calculate log returns
    let returns: Vec<f64> = recent
        .windows(2)
        .filter_map(|w| {
            let prev = w[0].close.to_f64()?;
            let curr = w[1].close.to_f64()?;
            if prev > 0.0 {
                Some((curr / prev).ln())
            } else {
                None
            }
        })
        .collect();

    if returns.is_empty() {
        return 0.003;
    }

    // Calculate standard deviation
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;

    variance.sqrt().max(0.0001)
}

// PLACEHOLDER_PAPER_SIGNAL

// ============================================================================
// Paper Trading Types (strategy-agnostic)
// ============================================================================

/// Paper trading signal record.
///
/// This is a strategy-agnostic signal record. Strategy-specific paper traders
/// (e.g., vol-arb) live in the main application crate and produce these records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PaperSignal {
    pub timestamp: DateTime<Utc>,
    pub symbol: String,
    pub market_id: String,
    pub condition_id: String,
    pub direction: String,
    pub entry_price: Decimal,
    pub fair_value: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub recommended_shares: u64,
    pub buffer_pct: Decimal,
    pub our_volatility: f64,
    pub implied_volatility: f64,
    pub time_remaining_secs: u64,
    // Resolution tracking
    pub resolution_time: Option<DateTime<Utc>>,
    pub actual_outcome: Option<bool>,
    pub would_have_won: Option<bool>,
    pub theoretical_pnl: Option<Decimal>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaperTradingStats {
    pub total_signals: u64,
    pub winning_signals: u64,
    pub win_rate: f64,
    pub theoretical_pnl: Decimal,
    pub avg_vol_edge: f64,
    pub avg_confidence: f64,
    pub pending_signals: u64,
}

// ============================================================================
// Report Generation
// ============================================================================

impl BacktestResults {
    /// Generate a text report
    pub fn report(&self) -> String {
        let mut report = String::new();

        report.push_str("+=============================================================+\n");
        report.push_str("|              BACKTEST REPORT                                 |\n");
        report.push_str("+=============================================================+\n");

        report.push_str(&format!(
            "| Period: {} to {}\n",
            self.start_time.format("%Y-%m-%d"),
            self.end_time.format("%Y-%m-%d")
        ));

        report.push_str("+-------------------------------------------------------------+\n");
        report.push_str("| PERFORMANCE SUMMARY                                          |\n");
        report.push_str("+-------------------------------------------------------------+\n");

        report.push_str(&format!(
            "| Total Trades:      {:>10}                              |\n",
            self.total_trades
        ));
        report.push_str(&format!(
            "| Winning Trades:    {:>10}                              |\n",
            self.winning_trades
        ));
        report.push_str(&format!(
            "| Win Rate:          {:>10.2}%                             |\n",
            self.win_rate * 100.0
        ));
        report.push_str(&format!(
            "| Total PnL:         ${:>9.2}                             |\n",
            self.total_pnl
        ));
        report.push_str(&format!(
            "| Total Volume:      ${:>9.2}                             |\n",
            self.total_volume
        ));
        report.push_str(&format!(
            "| Avg PnL/Trade:     ${:>9.2}                             |\n",
            self.avg_pnl_per_trade
        ));

        // PLACEHOLDER_REPORT_RISK

        report.push_str("+-------------------------------------------------------------+\n");
        report.push_str("| RISK METRICS                                                 |\n");
        report.push_str("+-------------------------------------------------------------+\n");

        report.push_str(&format!(
            "| Max Drawdown:      {:>10.2}%                             |\n",
            self.max_drawdown * dec!(100)
        ));
        report.push_str(&format!(
            "| Sharpe Ratio:      {:>10.2}                              |\n",
            self.sharpe_ratio
        ));
        report.push_str(&format!(
            "| Profit Factor:     {:>10.2}                              |\n",
            self.profit_factor
        ));

        report.push_str("+-------------------------------------------------------------+\n");
        report.push_str("| WIN/LOSS ANALYSIS                                            |\n");
        report.push_str("+-------------------------------------------------------------+\n");

        report.push_str(&format!(
            "| Average Win:       ${:>9.2}                             |\n",
            self.avg_win
        ));
        report.push_str(&format!(
            "| Average Loss:      ${:>9.2}                             |\n",
            self.avg_loss
        ));
        report.push_str(&format!(
            "| Largest Win:       ${:>9.2}                             |\n",
            self.largest_win
        ));
        report.push_str(&format!(
            "| Largest Loss:      ${:>9.2}                             |\n",
            self.largest_loss
        ));

        report.push_str("+-------------------------------------------------------------+\n");
        report.push_str("| BY SYMBOL                                                    |\n");
        report.push_str("+-------------------------------------------------------------+\n");

        for (symbol, stats) in &self.trades_by_symbol {
            report.push_str(&format!(
                "| {:8} | Trades: {:>4} | Win: {:>5.1}% | PnL: ${:>8.2}        |\n",
                symbol,
                stats.total_trades,
                stats.win_rate * 100.0,
                stats.total_pnl
            ));
        }

        report.push_str("+=============================================================+\n");

        report
    }

    /// Export results to JSON
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kline_volatility() {
        let klines = vec![
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(100),
                high: dec!(101),
                low: dec!(99),
                close: dec!(100),
                volume: dec!(1000),
            },
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(100),
                high: dec!(102),
                low: dec!(99),
                close: dec!(101),
                volume: dec!(1000),
            },
            KlineRecord {
                timestamp: Utc::now(),
                symbol: "BTCUSDT".into(),
                open: dec!(101),
                high: dec!(102),
                low: dec!(100),
                close: dec!(100.5),
                volume: dec!(1000),
            },
        ];

        let vol = calculate_kline_volatility(&klines, 12);
        assert!(vol > 0.0);
        assert!(vol < 0.1); // Should be reasonable
    }
}
