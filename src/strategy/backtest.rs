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
use tracing::info;

use crate::strategy::volatility_arb::{
    VolatilityArbConfig, VolatilityArbEngine, calculate_implied_volatility,
};

#[cfg(test)]
use rust_decimal_macros::dec;

mod loaders;
mod paper_trader;
mod reporting;
mod runtime;
mod statistics;

pub use loaders::{load_klines_from_csv, load_pm_prices_from_csv};
pub use paper_trader::{PaperSignal, PaperTrader, PaperTradingStats};
pub use statistics::{BacktestResults, BacktestTrade, SymbolStats};

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
