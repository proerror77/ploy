//! Trade logging and statistics tracking
//!
//! Provides persistent trade records and performance analytics:
//! - JSON file-based trade logging
//! - Per-symbol win rate and ROI tracking
//! - Historical performance analysis

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

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

/// Per-symbol statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SymbolStats {
    pub symbol: String,
    pub total_trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub open: u32,
    pub total_cost: Decimal,
    pub total_payout: Decimal,
    pub total_pnl: Decimal,
    pub avg_entry_price: Decimal,
    pub avg_edge: Decimal,
    pub last_trade: Option<DateTime<Utc>>,
}

impl SymbolStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.total_cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.total_pnl / self.total_cost
    }
}

/// Overall trading statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TradingStats {
    pub total_trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub open: u32,
    pub total_cost: Decimal,
    pub total_payout: Decimal,
    pub total_pnl: Decimal,
    pub by_symbol: HashMap<String, SymbolStats>,
    /// Stats by time bucket (0-2, 2-5, 5-10, 10-15)
    #[serde(default)]
    pub by_time_bucket: HashMap<String, BucketStats>,
    /// Stats by strategy mode (early_mispricing, late_reversal)
    #[serde(default)]
    pub by_strategy_mode: HashMap<String, BucketStats>,
}

/// Statistics for a time bucket or strategy mode
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BucketStats {
    pub trades: u32,
    pub wins: u32,
    pub losses: u32,
    pub cost: Decimal,
    pub pnl: Decimal,
}

impl BucketStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.pnl / self.cost
    }

    pub fn ev_per_trade(&self) -> Decimal {
        if self.trades == 0 {
            return Decimal::ZERO;
        }
        self.pnl / Decimal::from(self.trades)
    }
}

impl TradingStats {
    pub fn win_rate(&self) -> Decimal {
        let closed = self.wins + self.losses;
        if closed == 0 {
            return Decimal::ZERO;
        }
        Decimal::from(self.wins) / Decimal::from(closed)
    }

    pub fn roi(&self) -> Decimal {
        if self.total_cost == Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.total_pnl / self.total_cost
    }
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

    /// Load existing trades from file
    pub async fn load(&self) -> crate::error::Result<()> {
        if !self.log_path.exists() {
            debug!("No existing trades file, starting fresh");
            return Ok(());
        }

        let content = tokio::fs::read_to_string(&self.log_path).await?;
        let trades: Vec<TradeRecord> = serde_json::from_str(&content)?;

        info!("Loaded {} historical trades", trades.len());

        // Update cache
        {
            let mut cache = self.trades.write().await;
            *cache = trades;
        }

        // Recalculate stats
        self.recalculate_stats().await;

        Ok(())
    }

    /// Save trades to file
    pub async fn save(&self) -> crate::error::Result<()> {
        // Ensure directory exists
        if let Some(parent) = self.log_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let trades = self.trades.read().await;
        let content = serde_json::to_string_pretty(&*trades)?;
        tokio::fs::write(&self.log_path, content).await?;

        debug!("Saved {} trades to {:?}", trades.len(), self.log_path);
        Ok(())
    }

    /// Record a new trade entry (simple version)
    pub async fn record_entry(
        &self,
        symbol: &str,
        event_slug: &str,
        condition_id: &str,
        direction: &str,
        entry_price: Decimal,
        shares: u64,
        momentum_pct: Decimal,
        edge_pct: Decimal,
    ) -> String {
        self.record_entry_with_context(
            symbol,
            event_slug,
            condition_id,
            direction,
            entry_price,
            shares,
            momentum_pct,
            edge_pct,
            TradeContext::default(),
        )
        .await
    }

    /// Record a new trade entry with full market context
    pub async fn record_entry_with_context(
        &self,
        symbol: &str,
        event_slug: &str,
        condition_id: &str,
        direction: &str,
        entry_price: Decimal,
        shares: u64,
        momentum_pct: Decimal,
        edge_pct: Decimal,
        context: TradeContext,
    ) -> String {
        let id = format!("{}_{}", condition_id, Utc::now().timestamp_millis());
        let cost_usd = entry_price * Decimal::from(shares);

        let record = TradeRecord {
            id: id.clone(),
            timestamp: Utc::now(),
            symbol: symbol.to_string(),
            event_slug: event_slug.to_string(),
            condition_id: condition_id.to_string(),
            direction: direction.to_string(),
            entry_price,
            shares,
            cost_usd,
            momentum_pct,
            edge_pct,
            outcome: TradeOutcome::Open,
            payout_usd: None,
            pnl_usd: None,
            resolved_at: None,
            context,
        };

        info!(
            "📝 Trade logged: {} {} {} @ {:.2}¢ | {} shares = ${:.2}",
            symbol,
            direction,
            event_slug,
            entry_price * dec!(100),
            shares,
            cost_usd
        );

        {
            let mut trades = self.trades.write().await;
            trades.push(record);
        }

        // Update stats
        {
            let mut stats = self.stats.write().await;
            stats.total_trades += 1;
            stats.open += 1;
            stats.total_cost += cost_usd;

            let symbol_stats = stats
                .by_symbol
                .entry(symbol.to_string())
                .or_insert_with(|| SymbolStats {
                    symbol: symbol.to_string(),
                    ..Default::default()
                });
            symbol_stats.total_trades += 1;
            symbol_stats.open += 1;
            symbol_stats.total_cost += cost_usd;
            symbol_stats.last_trade = Some(Utc::now());
        }

        // Auto-save
        if let Err(e) = self.save().await {
            error!("Failed to save trades: {}", e);
        }

        id
    }

    /// Record trade resolution (win/loss)
    pub async fn record_resolution(&self, condition_id: &str, won: bool) {
        let mut trades = self.trades.write().await;

        // Find the trade
        if let Some(trade) = trades
            .iter_mut()
            .find(|t| t.condition_id == condition_id && t.outcome == TradeOutcome::Open)
        {
            let payout = if won {
                Decimal::from(trade.shares) // $1 per share
            } else {
                Decimal::ZERO
            };
            let pnl = payout - trade.cost_usd;

            trade.outcome = if won {
                TradeOutcome::Won
            } else {
                TradeOutcome::Lost
            };
            trade.payout_usd = Some(payout);
            trade.pnl_usd = Some(pnl);
            trade.resolved_at = Some(Utc::now());

            let symbol = trade.symbol.clone();

            info!(
                "📊 Trade resolved: {} {} {} | {} | PnL: ${:.2}",
                symbol,
                trade.direction,
                if won { "WON" } else { "LOST" },
                trade.event_slug,
                pnl
            );

            drop(trades);

            // Update stats
            {
                let mut stats = self.stats.write().await;
                stats.open = stats.open.saturating_sub(1);
                stats.total_payout += payout;
                stats.total_pnl += pnl;

                if won {
                    stats.wins += 1;
                } else {
                    stats.losses += 1;
                }

                if let Some(symbol_stats) = stats.by_symbol.get_mut(&symbol) {
                    symbol_stats.open = symbol_stats.open.saturating_sub(1);
                    symbol_stats.total_payout += payout;
                    symbol_stats.total_pnl += pnl;
                    if won {
                        symbol_stats.wins += 1;
                    } else {
                        symbol_stats.losses += 1;
                    }
                }
            }

            // Auto-save
            if let Err(e) = self.save().await {
                error!("Failed to save trades: {}", e);
            }
        } else {
            warn!("Trade not found for condition_id: {}", condition_id);
        }
    }

    /// Recalculate statistics from trades
    async fn recalculate_stats(&self) {
        let trades = self.trades.read().await;
        let mut stats = TradingStats::default();

        for trade in trades.iter() {
            stats.total_trades += 1;
            stats.total_cost += trade.cost_usd;

            let pnl = trade.pnl_usd.unwrap_or(Decimal::ZERO);
            let is_closed = matches!(&trade.outcome, TradeOutcome::Won | TradeOutcome::Lost);

            match &trade.outcome {
                TradeOutcome::Open => stats.open += 1,
                TradeOutcome::Won => {
                    stats.wins += 1;
                    stats.total_payout += trade.payout_usd.unwrap_or(Decimal::ZERO);
                    stats.total_pnl += pnl;
                }
                TradeOutcome::Lost => {
                    stats.losses += 1;
                    stats.total_pnl += pnl;
                }
                TradeOutcome::ExitedEarly { .. } | TradeOutcome::Cancelled => {}
            }

            // Per-symbol stats
            let symbol_stats = stats
                .by_symbol
                .entry(trade.symbol.clone())
                .or_insert_with(|| SymbolStats {
                    symbol: trade.symbol.clone(),
                    ..Default::default()
                });

            symbol_stats.total_trades += 1;
            symbol_stats.total_cost += trade.cost_usd;

            match &trade.outcome {
                TradeOutcome::Open => symbol_stats.open += 1,
                TradeOutcome::Won => {
                    symbol_stats.wins += 1;
                    symbol_stats.total_payout += trade.payout_usd.unwrap_or(Decimal::ZERO);
                    symbol_stats.total_pnl += pnl;
                }
                TradeOutcome::Lost => {
                    symbol_stats.losses += 1;
                    symbol_stats.total_pnl += pnl;
                }
                _ => {}
            }

            if symbol_stats
                .last_trade
                .map_or(true, |last| trade.timestamp > last)
            {
                symbol_stats.last_trade = Some(trade.timestamp);
            }

            // === Time Bucket Stats ===
            if let Some(ref bucket) = trade.context.time_bucket {
                let bucket_stats = stats
                    .by_time_bucket
                    .entry(bucket.clone())
                    .or_insert_with(BucketStats::default);

                bucket_stats.trades += 1;
                bucket_stats.cost += trade.cost_usd;

                if is_closed {
                    bucket_stats.pnl += pnl;
                    match &trade.outcome {
                        TradeOutcome::Won => bucket_stats.wins += 1,
                        TradeOutcome::Lost => bucket_stats.losses += 1,
                        _ => {}
                    }
                }
            }

            // === Strategy Mode Stats ===
            if let Some(ref mode) = trade.context.strategy_mode {
                let mode_stats = stats
                    .by_strategy_mode
                    .entry(mode.clone())
                    .or_insert_with(BucketStats::default);

                mode_stats.trades += 1;
                mode_stats.cost += trade.cost_usd;

                if is_closed {
                    mode_stats.pnl += pnl;
                    match &trade.outcome {
                        TradeOutcome::Won => mode_stats.wins += 1,
                        TradeOutcome::Lost => mode_stats.losses += 1,
                        _ => {}
                    }
                }
            }
        }

        let mut cached_stats = self.stats.write().await;
        *cached_stats = stats;
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
        let mut output = String::new();

        output.push_str("\n╔══════════════════════════════════════════════════════════════╗\n");
        output.push_str("║                    TRADING STATISTICS                        ║\n");
        output.push_str("╚══════════════════════════════════════════════════════════════╝\n\n");

        output.push_str(&format!("  Total Trades:  {}\n", stats.total_trades));
        output.push_str(&format!(
            "  Wins:          {} ({:.1}%)\n",
            stats.wins,
            stats.win_rate() * dec!(100)
        ));
        output.push_str(&format!("  Losses:        {}\n", stats.losses));
        output.push_str(&format!("  Open:          {}\n", stats.open));
        output.push_str(&format!("  Total Cost:    ${:.2}\n", stats.total_cost));
        output.push_str(&format!("  Total Payout:  ${:.2}\n", stats.total_payout));
        output.push_str(&format!("  Total PnL:     ${:.2}\n", stats.total_pnl));
        output.push_str(&format!(
            "  ROI:           {:.1}%\n",
            stats.roi() * dec!(100)
        ));

        // Per-symbol breakdown
        output.push_str("\n  ── Per Symbol ──────────────────────────────────────────────\n\n");
        output.push_str("  Symbol     Trades  Win%    PnL       ROI\n");
        output.push_str("  ────────   ──────  ──────  ────────  ────────\n");

        let mut symbols: Vec<_> = stats.by_symbol.values().collect();
        symbols.sort_by(|a, b| b.total_pnl.cmp(&a.total_pnl));

        for s in symbols {
            output.push_str(&format!(
                "  {:<10} {:>4}    {:>5.1}%  ${:>7.2}  {:>6.1}%\n",
                s.symbol,
                s.total_trades,
                s.win_rate() * dec!(100),
                s.total_pnl,
                s.roi() * dec!(100)
            ));
        }

        // Time bucket analysis
        if !stats.by_time_bucket.is_empty() {
            output.push_str("\n  ── By Entry Time (minutes elapsed) ─────────────────────────\n\n");
            output.push_str("  Bucket   Trades  Win%    PnL       EV/trade  ROI\n");
            output.push_str("  ───────  ──────  ──────  ────────  ────────  ────────\n");

            // Sort buckets in chronological order
            let bucket_order = ["0-2", "2-5", "5-10", "10-15"];
            for bucket in bucket_order.iter() {
                if let Some(b) = stats.by_time_bucket.get(*bucket) {
                    output.push_str(&format!(
                        "  {:<7}  {:>4}    {:>5.1}%  ${:>7.2}  ${:>6.2}   {:>6.1}%\n",
                        bucket,
                        b.trades,
                        b.win_rate() * dec!(100),
                        b.pnl,
                        b.ev_per_trade(),
                        b.roi() * dec!(100)
                    ));
                }
            }
        }

        // Strategy mode analysis
        if !stats.by_strategy_mode.is_empty() {
            output.push_str("\n  ── By Strategy Mode ────────────────────────────────────────\n\n");
            output.push_str("  Mode              Trades  Win%    PnL       EV/trade  ROI\n");
            output.push_str("  ───────────────── ──────  ──────  ────────  ────────  ────────\n");

            let mode_order = ["early_mispricing", "late_reversal"];
            for mode in mode_order.iter() {
                if let Some(m) = stats.by_strategy_mode.get(*mode) {
                    output.push_str(&format!(
                        "  {:<17} {:>4}    {:>5.1}%  ${:>7.2}  ${:>6.2}   {:>6.1}%\n",
                        mode,
                        m.trades,
                        m.win_rate() * dec!(100),
                        m.pnl,
                        m.ev_per_trade(),
                        m.roi() * dec!(100)
                    ));
                }
            }
        }

        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_stats() {
        let stats = SymbolStats {
            symbol: "BTCUSDT".to_string(),
            total_trades: 10,
            wins: 7,
            losses: 3,
            open: 0,
            total_cost: dec!(35),
            total_payout: dec!(70),
            total_pnl: dec!(35),
            ..Default::default()
        };

        assert_eq!(stats.win_rate(), dec!(0.7));
        assert_eq!(stats.roi(), dec!(1)); // 100% ROI
    }
}
