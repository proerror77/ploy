//! Backtest and Paper Trading Framework
//!
//! This module provides:
//! 1. Historical data loading from CSV/JSON files
//! 2. Backtest engine for strategy validation
//! 3. Paper trading mode for live signal recording without execution
//!
//! ## Usage
//!
//! ```bash
//! # Backtest volatility arbitrage strategy
//! ploy backtest vol-arb --data ./data/btc_2024.csv --start 2024-01-01 --end 2024-12-31
//!
//! # Paper trading mode
//! ploy paper-trade vol-arb --symbols BTC,ETH,SOL
//! ```

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::info;

use crate::strategy::volatility_arb::{
    calculate_implied_volatility, VolatilityArbConfig, VolatilityArbEngine,
};

#[cfg(test)]
use rust_decimal_macros::dec;

mod loaders;
mod paper_trader;
mod reporting;
mod runtime;

pub use loaders::{load_klines_from_csv, load_pm_prices_from_csv};
pub use paper_trader::{PaperSignal, PaperTrader, PaperTradingStats};

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

// ============================================================================
// Backtest Engine
// ============================================================================

pub struct BacktestEngine {
    config: VolatilityArbConfig,
    vol_engine: VolatilityArbEngine,
    results: BacktestResults,
    current_equity: Decimal,
    peak_equity: Decimal,
    initial_capital: Decimal,
}

impl BacktestEngine {
    pub fn new(config: VolatilityArbConfig, initial_capital: Decimal) -> Self {
        Self {
            vol_engine: VolatilityArbEngine::new(config.clone()),
            config,
            results: BacktestResults::default(),
            current_equity: initial_capital,
            peak_equity: initial_capital,
            initial_capital,
        }
    }

    /// Run backtest on historical data
    pub fn run(&mut self, klines: &[KlineRecord], pm_prices: &[PMPriceRecord]) -> BacktestResults {
        info!(
            "Starting backtest with {} klines and {} PM prices",
            klines.len(),
            pm_prices.len()
        );

        // Build volatility lookup by symbol and time
        let vol_lookup = self.build_volatility_lookup(klines);

        // Group PM prices by market
        let markets = self.group_by_market(pm_prices);

        // Track start/end times
        if let Some(first) = pm_prices.first() {
            self.results.start_time = first.timestamp;
        }
        if let Some(last) = pm_prices.last() {
            self.results.end_time = last.timestamp;
        }

        // Process each market
        for (market_id, prices) in markets {
            self.process_market(&market_id, &prices, &vol_lookup);
        }

        // Calculate final statistics
        self.calculate_statistics();

        self.results.clone()
    }

    fn calculate_statistics(&mut self) {
        let trades = &self.results.trades;

        if trades.is_empty() {
            return;
        }

        // Win rate
        self.results.win_rate =
            self.results.winning_trades as f64 / self.results.total_trades as f64;

        // Average PnL
        self.results.avg_pnl_per_trade =
            self.results.total_pnl / Decimal::from(self.results.total_trades);

        // Wins and losses
        let wins: Vec<_> = trades.iter().filter(|t| t.won).collect();
        let losses: Vec<_> = trades.iter().filter(|t| !t.won).collect();

        if !wins.is_empty() {
            self.results.avg_win =
                wins.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(wins.len() as u64);
            self.results.largest_win = wins.iter().map(|t| t.pnl).max().unwrap_or(Decimal::ZERO);
        }

        if !losses.is_empty() {
            self.results.avg_loss =
                losses.iter().map(|t| t.pnl).sum::<Decimal>() / Decimal::from(losses.len() as u64);
            self.results.largest_loss = losses.iter().map(|t| t.pnl).min().unwrap_or(Decimal::ZERO);
        }

        // Max drawdown
        let mut peak = self.initial_capital;
        let mut max_dd = Decimal::ZERO;

        for (_, equity) in &self.results.equity_curve {
            if *equity > peak {
                peak = *equity;
            }
            let dd = (peak - equity) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
        self.results.max_drawdown = max_dd;

        // Profit factor
        let total_wins: Decimal = wins.iter().map(|t| t.pnl).sum();
        let total_losses: Decimal = losses.iter().map(|t| t.pnl.abs()).sum();

        if total_losses > Decimal::ZERO {
            self.results.profit_factor = (total_wins / total_losses).to_f64().unwrap_or(0.0);
        }

        // Sharpe ratio (simplified)
        let returns: Vec<f64> = trades.iter().filter_map(|t| t.pnl_pct.to_f64()).collect();

        if returns.len() > 1 {
            let mean = returns.iter().sum::<f64>() / returns.len() as f64;
            let variance =
                returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
            let std_dev = variance.sqrt();

            if std_dev > 0.0 {
                // Annualized: assume ~100 trades per year
                self.results.sharpe_ratio = mean / std_dev * (100.0_f64).sqrt();
            }
        }

        // Average holding time
        let total_hold_time: i64 = trades
            .iter()
            .map(|t| (t.exit_time - t.entry_time).num_seconds())
            .sum();
        self.results.avg_holding_time_secs = total_hold_time as f64 / trades.len() as f64;

        // Update symbol win rates
        for stats in self.results.trades_by_symbol.values_mut() {
            if stats.total_trades > 0 {
                stats.win_rate = stats.winning_trades as f64 / stats.total_trades as f64;
            }
        }
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

    #[test]
    fn test_paper_trader() {
        let config = VolatilityArbConfig::default();
        let mut trader = PaperTrader::new(config, None);

        // Set volatility
        trader.update_volatility("BTCUSDT", 0.003);

        // Check for signal
        let signal = trader.check_and_record(
            "BTCUSDT",
            "market_123",
            "condition_456",
            dec!(94500),
            dec!(94000),
            dec!(0.70),
            dec!(0.71),
            300,
            Some(0.0025),
        );

        // May or may not generate signal depending on edge
        println!("Signal: {:?}", signal);

        // Get stats
        let stats = trader.statistics();
        println!("Stats: {:?}", stats);
    }
}
