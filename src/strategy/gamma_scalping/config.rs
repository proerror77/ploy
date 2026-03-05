//! Configuration for the gamma scalping strategy.

use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Configuration for gamma scalping on Polymarket crypto binaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GammaScalpingConfig {
    /// Strategy identifier
    pub id: String,
    /// Whether the strategy is enabled
    pub enabled: bool,
    /// Binance symbols to track (e.g., ["BTCUSDT", "ETHUSDT", "SOLUSDT"])
    pub symbols: Vec<String>,
    /// Polymarket series IDs for event discovery
    pub series_ids: Vec<String>,

    // --- Vol edge thresholds ---
    /// Minimum realized/implied vol ratio to enter (e.g., 0.15 = 15% edge)
    pub min_vol_edge_pct: f64,
    /// Number of kline periods for realized vol calculation
    pub vol_lookback_periods: usize,
    /// Kline interval for vol estimation (e.g., "1m")
    pub kline_interval: String,

    // --- Rebalancing ---
    /// Rebalance when |portfolio delta| exceeds this threshold
    pub rebalance_delta_threshold: f64,
    /// Minimum seconds between rebalances
    pub rebalance_interval_secs: u64,

    // --- Entry/exit timing ---
    /// Don't enter with less than N seconds remaining
    pub min_time_remaining_secs: u64,
    /// Don't enter with more than N seconds remaining
    pub max_time_remaining_secs: u64,
    /// Close all positions N seconds before expiry
    pub exit_before_expiry_secs: u64,

    // --- Position sizing ---
    /// Max USD per straddle
    pub max_position_usd: Decimal,
    /// Kelly fraction scaling (e.g., 0.25 = quarter Kelly)
    pub kelly_fraction: f64,
    /// Max concurrent straddles
    pub max_positions: usize,

    // --- Risk ---
    /// Maximum daily loss before halting
    pub max_daily_loss_usd: Decimal,
    /// Cap on total portfolio gamma exposure
    pub max_gamma_exposure: f64,
    /// Cooldown seconds after closing a straddle
    pub cooldown_secs: u64,

    /// Dry run mode (log signals, don't submit orders)
    pub dry_run: bool,
}

impl Default for GammaScalpingConfig {
    fn default() -> Self {
        Self {
            id: "gamma_scalping".to_string(),
            enabled: true,
            symbols: vec![
                "BTCUSDT".to_string(),
                "ETHUSDT".to_string(),
                "SOLUSDT".to_string(),
            ],
            series_ids: Vec::new(),
            min_vol_edge_pct: 0.15,
            vol_lookback_periods: 12,
            kline_interval: "1m".to_string(),
            rebalance_delta_threshold: 0.15,
            rebalance_interval_secs: 15,
            min_time_remaining_secs: 120,
            max_time_remaining_secs: 600,
            exit_before_expiry_secs: 60,
            max_position_usd: dec!(10),
            kelly_fraction: 0.25,
            max_positions: 3,
            max_daily_loss_usd: dec!(30),
            max_gamma_exposure: 50.0,
            cooldown_secs: 60,
            dry_run: true,
        }
    }
}
