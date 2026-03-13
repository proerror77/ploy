//! Trade logging and statistics tracking
//!
//! Provides persistent trade records and performance analytics:
//! - JSON file-based trade logging
//! - Per-symbol win rate and ROI tracking
//! - Historical performance analysis

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;

mod summary;
mod stats;
mod stats_rebuild;
mod write_flow;

pub use stats::{BucketStats, SymbolStats, TradingStats};

/// Trade record for logging with full market context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeRecord {
    /// Unique trade ID
    pub id: String,
    /// Timestamp of trade
    pub timestamp: DateTime<Utc>,
    /// Trading symbol (e.g., BTCUSDT)
    pub symbol: String,
    /// Event slug
    pub event_slug: String,
    /// Condition ID
    pub condition_id: String,
    /// Direction (Up/Down)
    pub direction: String,
    /// Entry price (0-1)
    pub entry_price: Decimal,
    /// Number of shares
    pub shares: u64,
    /// Cost in USD
    pub cost_usd: Decimal,
    /// CEX momentum at entry
    pub momentum_pct: Decimal,
    /// Estimated edge at entry
    pub edge_pct: Decimal,
    /// Trade outcome
    pub outcome: TradeOutcome,
    /// Payout received (if won)
    pub payout_usd: Option<Decimal>,
    /// Profit/loss
    pub pnl_usd: Option<Decimal>,
    /// Resolution timestamp
    pub resolved_at: Option<DateTime<Utc>>,

    // === Enhanced Market Context ===
    /// Market context at entry time
    #[serde(default)]
    pub context: TradeContext,
}

/// Detailed market context at trade entry
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TradeContext {
    // === Polymarket Order Book ===
    /// Best bid price
    pub bid_price: Option<Decimal>,
    /// Best ask price
    pub ask_price: Option<Decimal>,
    /// Bid-ask spread in cents
    pub spread_cents: Option<Decimal>,
    /// Bid depth (shares at best bid)
    pub bid_depth: Option<u64>,
    /// Ask depth (shares at best ask)
    pub ask_depth: Option<u64>,

    // === Time Context ===
    /// Seconds remaining until resolution
    pub time_remaining_secs: Option<i64>,
    /// Minutes into the 15-minute window (0-15)
    pub minutes_elapsed: Option<u32>,
    /// Time bucket for analysis (0-2, 2-5, 5-10, 10-15)
    pub time_bucket: Option<String>,

    // === CEX Spot Context ===
    /// CEX spot price at entry
    pub spot_price: Option<Decimal>,
    /// Spot price 1s ago
    pub spot_1s_ago: Option<Decimal>,
    /// Spot price 5s ago
    pub spot_5s_ago: Option<Decimal>,
    /// Spot price 30s ago
    pub spot_30s_ago: Option<Decimal>,
    /// Spot price 60s ago
    pub spot_60s_ago: Option<Decimal>,
    /// Spot price at event start
    pub spot_at_start: Option<Decimal>,
    /// Price change from event start
    pub move_from_start_pct: Option<Decimal>,

    // === Signal Context ===
    /// Multi-timeframe momentums
    pub momentum_10s: Option<Decimal>,
    pub momentum_30s: Option<Decimal>,
    pub momentum_60s: Option<Decimal>,
    /// Current volatility
    pub volatility: Option<Decimal>,
    /// Baseline volatility for this symbol
    pub baseline_volatility: Option<Decimal>,
    /// Volatility ratio (current / baseline)
    pub volatility_ratio: Option<Decimal>,
    /// Signal confidence score
    pub confidence: Option<f64>,

    // === Strategy Mode ===
    /// Strategy type: "early_mispricing" or "late_reversal"
    pub strategy_mode: Option<String>,
}

impl TradeContext {
    /// Calculate time bucket from minutes elapsed (0-2, 2-5, 5-10, 10-15)
    pub fn time_bucket_from_minutes(minutes: u32) -> String {
        match minutes {
            0..=2 => "0-2".to_string(),
            3..=5 => "2-5".to_string(),
            6..=10 => "5-10".to_string(),
            _ => "10-15".to_string(),
        }
    }

    /// Determine strategy mode from time remaining
    /// Early mispricing: >5 min remaining (0-10 min elapsed)
    /// Late reversal: <5 min remaining (10-15 min elapsed)
    pub fn strategy_mode_from_minutes(minutes: u32) -> String {
        if minutes <= 10 {
            "early_mispricing".to_string()
        } else {
            "late_reversal".to_string()
        }
    }

    /// Create context with time info
    pub fn with_time(time_remaining_secs: i64) -> Self {
        let minutes_elapsed = ((15 * 60 - time_remaining_secs) / 60).max(0) as u32;
        let time_bucket = Self::time_bucket_from_minutes(minutes_elapsed);
        let strategy_mode = Self::strategy_mode_from_minutes(minutes_elapsed);

        Self {
            time_remaining_secs: Some(time_remaining_secs),
            minutes_elapsed: Some(minutes_elapsed),
            time_bucket: Some(time_bucket),
            strategy_mode: Some(strategy_mode),
            ..Default::default()
        }
    }
}

/// Trade outcome
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TradeOutcome {
    /// Trade is still open
    Open,
    /// Won - collected $1 per share
    Won,
    /// Lost - lost entry cost
    Lost,
    /// Exited early (take profit / stop loss)
    ExitedEarly { exit_price: Decimal },
    /// Cancelled
    Cancelled,
}

/// Trade logger for persistent trade records
pub struct TradeLogger {
    /// Path to trades JSON file
    log_path: PathBuf,
    /// In-memory trade cache
    trades: RwLock<Vec<TradeRecord>>,
    /// Cached statistics
    stats: RwLock<TradingStats>,
}

impl TradeLogger {
    /// Create a new trade logger
    pub fn new(log_path: PathBuf) -> Self {
        Self {
            log_path,
            trades: RwLock::new(Vec::new()),
            stats: RwLock::new(TradingStats::default()),
        }
    }

    /// Create with default path (./data/trades.json)
    pub fn default_path() -> Self {
        let path = PathBuf::from("data/trades.json");
        Self::new(path)
    }

    /// Get current statistics
    pub async fn get_stats(&self) -> TradingStats {
        self.stats.read().await.clone()
    }

    /// Get recent trades
    pub async fn get_recent_trades(&self, limit: usize) -> Vec<TradeRecord> {
        let trades = self.trades.read().await;
        trades.iter().rev().take(limit).cloned().collect()
    }

    /// Get trades for a specific symbol
    pub async fn get_trades_by_symbol(&self, symbol: &str) -> Vec<TradeRecord> {
        let trades = self.trades.read().await;
        trades
            .iter()
            .filter(|t| t.symbol == symbol)
            .cloned()
            .collect()
    }

    /// Get open trades
    pub async fn get_open_trades(&self) -> Vec<TradeRecord> {
        let trades = self.trades.read().await;
        trades
            .iter()
            .filter(|t| t.outcome == TradeOutcome::Open)
            .cloned()
            .collect()
    }

    /// Get number of active symbols (with at least 1 trade)
    pub async fn get_active_symbol_count(&self) -> usize {
        let stats = self.stats.read().await;
        stats.by_symbol.len()
    }

    /// Format statistics for display
    pub async fn format_stats(&self) -> String {
        let stats = self.get_stats().await;
        summary::format_stats(&stats)
    }
}
