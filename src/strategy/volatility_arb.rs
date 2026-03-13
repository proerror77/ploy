//! Volatility Arbitrage Strategy
//!
//! This strategy exploits mispricing in Polymarket 15-minute crypto binary options
//! by comparing market-implied volatility with our estimated realized volatility.
//!
//! ## Mathematical Foundation
//!
//! For a binary option paying $1 if S > K at expiration:
//!
//! ```text
//! P(YES) = N(d2)
//! d2 = [ln(S/K) - σ²T/2] / (σ√T)
//!
//! Simplified for small buffer:
//! d2 ≈ buffer% / (σ × √T)
//! ```
//!
//! ## Edge Source
//!
//! If market prices YES at $0.70 implying σ_implied = 0.4%
//! But our estimate is σ_realized = 0.25%
//! Then YES is underpriced → BUY YES
//!
//! ## Key Insight
//!
//! We're not predicting direction. We're predicting VOLATILITY.
//! If we estimate volatility better than the market, we have edge.

use chrono::{DateTime, Utc};
use rust_decimal::prelude::*;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

pub use analysis::{
    MarketPricing, VolArbSignal, VolatilityEstimate, calculate_fair_yes_price,
    calculate_implied_volatility, calculate_kelly_fraction, norm_cdf,
};

mod config_defaults {
    pub(super) fn high_vol_threshold() -> f64 {
        0.005
    }

    pub(super) fn high_vol_kelly_multiplier() -> f64 {
        0.7
    }
}

mod analysis;
mod trade_lifecycle;

// ============================================================================
// Configuration
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolatilityArbConfig {
    // === Volatility Estimation Weights ===
    /// Weight for K-line historical volatility (0.0 - 1.0)
    pub kline_weight: f64,
    /// Weight for tick-based volatility (0.0 - 1.0)
    pub tick_weight: f64,
    /// Number of K-line periods to use for vol estimation
    pub vol_lookback_periods: usize,

    // === Trading Thresholds ===
    /// Minimum volatility edge to trade (e.g., 0.20 = 20% vol difference)
    pub min_vol_edge_pct: f64,
    /// Minimum buffer from threshold to trade (avoid coin-flip situations)
    pub min_buffer_pct: Decimal,
    /// Maximum buffer from threshold (no edge when outcome is certain)
    pub max_buffer_pct: Decimal,
    /// Minimum price edge after fees to trade
    pub min_price_edge: Decimal,

    // === Time Windows ===
    /// Minimum seconds remaining to trade
    pub min_time_remaining_secs: u64,
    /// Maximum seconds remaining to trade
    pub max_time_remaining_secs: u64,
    /// Optimal time window for trading (highest edge)
    pub optimal_time_range: (u64, u64),

    // === Risk Management ===
    /// Maximum position size in USD per trade
    pub max_position_usd: Decimal,
    /// Kelly fraction for position sizing (0.25 = quarter Kelly)
    pub kelly_fraction: f64,
    /// Combined volatility level above which we reduce Kelly sizing
    #[serde(default = "config_defaults::high_vol_threshold")]
    pub high_vol_threshold: f64,
    /// Multiplier applied to Kelly sizing in high volatility regimes
    #[serde(default = "config_defaults::high_vol_kelly_multiplier")]
    pub high_vol_kelly_multiplier: f64,
    /// Maximum total exposure per symbol
    pub max_symbol_exposure_usd: Decimal,
    /// Cooldown between trades on same market
    pub cooldown_secs: u64,

    // === Fee Structure ===
    /// Polymarket trading fee rate
    pub pm_fee_rate: Decimal,

    // === Symbols to Trade ===
    pub symbols: Vec<String>,
}

impl Default for VolatilityArbConfig {
    fn default() -> Self {
        Self {
            // Volatility estimation: 70% K-line, 30% tick
            kline_weight: 0.70,
            tick_weight: 0.30,
            vol_lookback_periods: 12, // 12 x 15-min = 3 hours

            // Trading thresholds
            min_vol_edge_pct: 0.15,      // 15% volatility edge minimum
            min_buffer_pct: dec!(0.001), // 0.1% minimum buffer
            max_buffer_pct: dec!(0.02),  // 2% maximum buffer
            min_price_edge: dec!(0.03),  // 3% price edge minimum

            // Time windows (seconds)
            min_time_remaining_secs: 120,   // 2 minutes minimum
            max_time_remaining_secs: 600,   // 10 minutes maximum
            optimal_time_range: (180, 420), // 3-7 minutes optimal

            // Risk management
            max_position_usd: dec!(50),
            kelly_fraction: 0.25, // Quarter Kelly
            high_vol_threshold: config_defaults::high_vol_threshold(),
            high_vol_kelly_multiplier: config_defaults::high_vol_kelly_multiplier(),
            max_symbol_exposure_usd: dec!(100),
            cooldown_secs: 300, // 5 minute cooldown

            // Fees
            pm_fee_rate: dec!(0.02),

            // Default symbols
            symbols: vec!["BTCUSDT".into(), "ETHUSDT".into(), "SOLUSDT".into()],
        }
    }
}

// ============================================================================
// Core Types
// ============================================================================

/// Trade outcome for tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolArbTrade {
    pub signal: VolArbSignalRecord,
    pub entry_price: Decimal,
    pub exit_price: Option<Decimal>,
    pub shares: u64,
    pub pnl: Option<Decimal>,
    pub outcome: Option<bool>, // true = won, false = lost
    pub entry_time: DateTime<Utc>,
    pub exit_time: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolArbSignalRecord {
    pub symbol: String,
    pub buy_yes: bool,
    pub fair_value: Decimal,
    pub market_price: Decimal,
    pub price_edge: Decimal,
    pub vol_edge_pct: f64,
    pub confidence: f64,
    pub buffer_pct: Decimal,
    pub time_remaining_secs: u64,
}

/// Strategy statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolArbStats {
    pub total_trades: u64,
    pub winning_trades: u64,
    pub total_pnl: Decimal,
    pub total_volume: Decimal,
    pub avg_edge: f64,
    pub avg_vol_edge: f64,
    pub win_rate: f64,
    pub sharpe_ratio: f64,
    pub trades_by_symbol: HashMap<String, u64>,
    pub pnl_by_symbol: HashMap<String, Decimal>,
}

// ============================================================================
// Mathematical Functions
// ============================================================================

// ============================================================================
// Volatility Arbitrage Engine
// ============================================================================

pub struct VolatilityArbEngine {
    config: VolatilityArbConfig,
    /// K-line volatility cache: symbol -> 15-min volatility
    kline_vol_cache: HashMap<String, f64>,
    /// Recent trades for tracking
    recent_trades: Vec<VolArbTrade>,
    /// Last trade time per market
    last_trade_time: HashMap<String, DateTime<Utc>>,
    /// Current positions
    positions: HashMap<String, VolArbPosition>,
    /// Statistics
    stats: VolArbStats,
}

#[derive(Debug, Clone)]
pub struct VolArbPosition {
    pub market_id: String,
    pub condition_id: String,
    pub symbol: String,
    pub is_yes: bool,
    pub shares: u64,
    pub entry_price: Decimal,
    pub entry_time: DateTime<Utc>,
    pub signal: VolArbSignalRecord,
}

impl VolatilityArbEngine {
    pub fn new(config: VolatilityArbConfig) -> Self {
        Self {
            config,
            kline_vol_cache: HashMap::new(),
            recent_trades: Vec::new(),
            last_trade_time: HashMap::new(),
            positions: HashMap::new(),
            stats: VolArbStats::default(),
        }
    }

    /// Update K-line volatility for a symbol
    pub fn update_kline_volatility(&mut self, symbol: &str, volatility: f64) {
        self.kline_vol_cache.insert(symbol.to_string(), volatility);
        debug!(symbol, volatility, "Updated K-line volatility");
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_norm_cdf() {
        // Test standard values
        assert!((norm_cdf(0.0) - 0.5).abs() < 0.001);
        assert!((norm_cdf(1.0) - 0.8413).abs() < 0.001);
        assert!((norm_cdf(-1.0) - 0.1587).abs() < 0.001);
        assert!((norm_cdf(2.0) - 0.9772).abs() < 0.001);
    }

    #[test]
    fn test_fair_yes_price() {
        // Buffer = 1%, Vol = 0.3%, Full time remaining
        // d2 = 0.01 / 0.003 = 3.33
        // N(3.33) ≈ 0.9996
        let price = calculate_fair_yes_price(0.01, 0.003, 1.0);
        assert!(price > 0.99);

        // Buffer = 0.1%, Vol = 0.3%, Full time
        // d2 = 0.001 / 0.003 = 0.33
        // N(0.33) ≈ 0.63
        let price = calculate_fair_yes_price(0.001, 0.003, 1.0);
        assert!(price > 0.6 && price < 0.7);

        // Negative buffer (below threshold)
        let price = calculate_fair_yes_price(-0.01, 0.003, 1.0);
        assert!(price < 0.01);
    }

    #[test]
    fn test_implied_volatility() {
        // Fair price 0.7, buffer 0.5%, half time remaining
        let implied = calculate_implied_volatility(0.7, 0.005, 0.5);
        assert!(implied.is_some());
        let vol = implied.unwrap();

        // Verify by calculating price back
        let price = calculate_fair_yes_price(0.005, vol, 0.5);
        assert!((price - 0.7).abs() < 0.01);
    }

    #[test]
    fn test_kelly_fraction() {
        // 60% win prob, entry at 0.50 (even odds)
        // b = 0.50 / 0.50 = 1.0
        // f = (0.6 * 1 - 0.4) / 1 = 0.2
        let kelly = calculate_kelly_fraction(0.6, 0.5);
        assert!((kelly - 0.2).abs() < 0.01);

        // 70% win prob, entry at 0.60
        // b = 0.40 / 0.60 = 0.667
        // f = (0.7 * 0.667 - 0.3) / 0.667 = 0.25
        let kelly = calculate_kelly_fraction(0.7, 0.6);
        assert!(kelly > 0.2 && kelly < 0.3);

        // No edge case
        let kelly = calculate_kelly_fraction(0.5, 0.5);
        assert!(kelly.abs() < 0.01);
    }

    #[test]
    fn test_vol_arb_engine() {
        let config = VolatilityArbConfig::default();
        let mut engine = VolatilityArbEngine::new(config);

        // Set up volatility
        engine.update_kline_volatility("BTCUSDT", 0.003);

        // Test signal generation
        let signal = engine.analyze_market(
            "BTCUSDT",
            "market_123",
            "condition_456",
            dec!(94500),  // Spot
            dec!(94000),  // Threshold
            dec!(0.70),   // YES price
            dec!(0.71),   // YES ask
            300,          // 5 minutes remaining
            Some(0.0025), // Tick volatility
        );

        // Signal should exist if there's vol edge
        // (depends on whether implied vol differs enough from our estimate)
        println!("Signal: {:?}", signal);
    }
}
