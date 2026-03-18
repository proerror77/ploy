use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Momentum strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumConfig {
    /// Minimum CEX price move to trigger (e.g., 0.003 = 0.3%)
    /// This is the BASE threshold, adjusted by volatility
    pub min_move_pct: Decimal,

    /// Maximum Polymarket odds for entry (e.g., 0.40 = 40¢)
    /// Lower = better entry like CRYINGLITTLEBABY style (20-30¢)
    pub max_entry_price: Decimal,

    /// Minimum estimated edge to enter (e.g., 0.03 = 3%)
    pub min_edge: Decimal,

    /// Lookback window for momentum calculation (seconds)
    /// Used as fallback when weighted momentum has insufficient history
    pub lookback_secs: u64,

    /// Use volatility-adjusted thresholds
    /// threshold = min_move_pct * (current_vol / baseline_vol)
    pub use_volatility_adjustment: bool,

    /// Baseline volatility for threshold adjustment (60s rolling std dev)
    /// BTC: ~0.0005 (0.05%), ETH: ~0.0008 (0.08%), SOL: ~0.0015 (0.15%)
    pub baseline_volatility: HashMap<String, Decimal>,

    /// Volatility lookback window in seconds
    pub volatility_lookback_secs: u64,

    /// Shares per trade
    pub shares_per_trade: u64,

    /// Maximum concurrent positions
    pub max_positions: usize,

    /// Cooldown between trades on same symbol (seconds)
    pub cooldown_secs: u64,

    /// Maximum trades per day (0 = unlimited)
    pub max_daily_trades: u32,

    /// Symbols to track
    pub symbols: Vec<String>,

    // === CRYINGLITTLEBABY CONFIRMATORY MODE ===
    /// Hold positions to resolution (don't exit early)
    /// When true: buy confirmed winners, collect $1 at resolution
    /// When false: use take-profit/stop-loss exits
    pub hold_to_resolution: bool,

    /// Minimum time remaining to enter (seconds)
    /// CRYINGLITTLEBABY style: 60s (1 min minimum)
    pub min_time_remaining_secs: u64,

    /// Maximum time remaining to enter (seconds)
    /// CRYINGLITTLEBABY style: 300s (5 min maximum)
    /// This ensures we only enter when outcome is nearly decided
    pub max_time_remaining_secs: u64,

    // === CROSS-SYMBOL RISK CONTROL ===
    /// Maximum total exposure across all symbols per 15-min window (USD)
    /// Set to 0 for unlimited
    pub max_window_exposure_usd: Decimal,

    /// Only enter the highest edge signal per 15-min window
    /// When true: queues signals and selects best edge after delay
    pub best_edge_only: bool,

    /// Delay before selecting best edge (milliseconds)
    /// Allow signals from all symbols to arrive before deciding
    pub signal_collection_delay_ms: u64,

    // === ENHANCED MOMENTUM DETECTION ===
    /// Require all timeframes (10s, 30s, 60s) to agree on direction
    /// When false: use weighted average (original behavior)
    /// When true: all must be same direction
    pub require_mtf_agreement: bool,

    /// Minimum OBI (Order Book Imbalance) for confirmation
    /// 0.0 = disabled, 0.1 = require 10% imbalance in signal direction
    pub min_obi_confirmation: Decimal,

    /// Use K-line historical volatility instead of tick-based
    /// More stable but less responsive
    pub use_kline_volatility: bool,

    /// Time decay factor: reduce signal strength as event progresses
    /// 0.0 = no decay, 1.0 = full decay (signal_strength * time_remaining/900)
    pub time_decay_factor: Decimal,

    /// Consider price-to-beat in fair value calculation
    /// When true: adjust fair value based on how close CEX price is to threshold
    pub use_price_to_beat: bool,

    /// Dynamic position sizing based on signal confidence
    /// When true: shares = base_shares * confidence
    pub dynamic_position_sizing: bool,

    /// Minimum confidence for entry (0.0 - 1.0)
    pub min_confidence: f64,

    /// Enable Kelly-based position scaling.
    /// Final shares are scaled by (kelly_fraction / kelly_fraction_cap), capped at 1.0.
    pub use_kelly_sizing: bool,

    /// Cap used to normalize Kelly sizing (e.g., 0.25 = quarter-Kelly cap).
    pub kelly_fraction_cap: Decimal,

    // === VWAP CONFIRMATION ===
    /// Require spot price direction to agree with VWAP.
    ///
    /// When true:
    /// - Up signals require: spot_price >= VWAP * (1 + min_vwap_deviation)
    /// - Down signals require: spot_price <= VWAP * (1 - min_vwap_deviation)
    pub require_vwap_confirmation: bool,

    /// VWAP lookback window (seconds).
    pub vwap_lookback_secs: u64,

    /// Minimum deviation from VWAP required for confirmation (e.g., 0.001 = 0.1%).
    pub min_vwap_deviation: Decimal,

    // === DIRECTIONAL MODE (BINANCE AS ORACLE) ===
    /// When true, on_cex_update() uses estimate_probability() + FeeModel
    /// instead of MomentumDetector/VolatilityDetector. Binance acts as
    /// Chainlink proxy for the log-normal probability model.
    pub directional_mode: bool,

    /// Volatility floor for probability model (directional mode only).
    pub directional_vol_floor: f64,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        // Default baseline volatility (60s rolling std dev)
        let mut baseline_volatility = HashMap::new();
        baseline_volatility.insert("BTCUSDT".into(), dec!(0.0005)); // 0.05%
        baseline_volatility.insert("ETHUSDT".into(), dec!(0.0008)); // 0.08%
        baseline_volatility.insert("SOLUSDT".into(), dec!(0.0015)); // 0.15%
        baseline_volatility.insert("XRPUSDT".into(), dec!(0.0012)); // 0.12%

        Self {
            // === AGGRESSIVE ENTRY (CRYINGLITTLEBABY style) ===
            min_move_pct: dec!(0.0005), // 0.05% base minimum move (adjusted by volatility)
            max_entry_price: dec!(0.35), // Max 35¢ entry (confirmed winner should be cheap)
            min_edge: dec!(0.03),       // 3% minimum edge
            lookback_secs: 5,           // 5-second fallback window

            // === Multi-timeframe momentum (always enabled) ===
            use_volatility_adjustment: true, // Adjust threshold by current volatility
            baseline_volatility,
            volatility_lookback_secs: 60, // 60-second rolling volatility

            shares_per_trade: 100, // ~$35 per trade at 35¢

            // === ANTI-OVERTRADING CONTROLS ===
            max_positions: 3,     // Max 3 concurrent
            cooldown_secs: 60,    // 60s between same symbol
            max_daily_trades: 20, // Max 20 trades/day

            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "XRPUSDT".into(),
            ],

            // === CRYINGLITTLEBABY CONFIRMATORY MODE (DEFAULT: ON) ===
            hold_to_resolution: true,     // Hold to collect $1
            min_time_remaining_secs: 60,  // Min 1 min left
            max_time_remaining_secs: 300, // Max 5 min left (outcome should be decided)

            // === CROSS-SYMBOL RISK CONTROL ===
            max_window_exposure_usd: dec!(25), // Max $25 total per 15-min window
            best_edge_only: true,              // Only take highest edge signal
            signal_collection_delay_ms: 2000,  // 2 second delay to collect signals

            // === ENHANCED MOMENTUM DETECTION ===
            require_mtf_agreement: true, // Require all timeframes to agree
            min_obi_confirmation: dec!(0.05), // 5% OBI confirmation
            use_kline_volatility: true,  // Use K-line historical volatility
            time_decay_factor: dec!(0.3), // 30% time decay
            use_price_to_beat: true,     // Consider price-to-beat
            dynamic_position_sizing: true, // Scale by confidence
            min_confidence: 0.5,         // Min 50% confidence
            use_kelly_sizing: true,      // Scale by empirical Kelly
            kelly_fraction_cap: dec!(0.25), // Quarter-Kelly cap

            // === VWAP CONFIRMATION (DEFAULT: OFF) ===
            require_vwap_confirmation: false,
            vwap_lookback_secs: 60,
            min_vwap_deviation: dec!(0),

            // === DIRECTIONAL MODE (DEFAULT: OFF) ===
            directional_mode: false,
            directional_vol_floor: 0.005,
        }
    }
}

/// Exit strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExitConfig {
    /// Take profit when price increases by this % (e.g., 0.20 = 20%)
    pub take_profit_pct: Decimal,

    /// Stop loss when price drops by this % (e.g., 0.15 = 15%)
    pub stop_loss_pct: Decimal,

    /// Trailing stop: lock in gains as price rises (e.g., 0.10 = 10%)
    pub trailing_stop_pct: Decimal,

    /// Force exit N seconds before resolution
    pub exit_before_resolution_secs: u64,
}

impl Default for ExitConfig {
    fn default() -> Self {
        Self {
            take_profit_pct: dec!(0.20),     // +20% take profit
            stop_loss_pct: dec!(0.15),       // -15% stop loss
            trailing_stop_pct: dec!(0.10),   // 10% trailing from high
            exit_before_resolution_secs: 30, // Exit 30s before end
        }
    }
}
