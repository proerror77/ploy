use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};

/// Configuration for a staggered arbitrage backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaggeredArbBacktestConfig {
    /// Symbols to backtest (e.g. ["BTCUSDT", "ETHUSDT"])
    pub symbols: Vec<String>,
    /// Starting equity in USD
    pub initial_capital: Decimal,
    /// Position size in shares per trade
    pub shares_per_trade: u64,
    /// Maximum concurrent Leg1 positions
    pub max_concurrent_positions: usize,
    // Signal thresholds
    /// Minimum |p_hat - 0.5| to trigger entry
    pub direction_threshold: f64,
    /// Maximum up_ask + down_ask to consider entry
    pub max_initial_sum: Decimal,
    /// Minimum profit target per share after both legs
    pub min_profit_target: Decimal,
    // Time control
    /// Maximum seconds to wait for Leg2 after Leg1 fill
    pub max_wait_secs: u64,
    /// Maximum fraction of window duration to wait for Leg2
    pub max_wait_pct: f64,
    /// Minimum time remaining in window to enter
    pub min_time_remaining_secs: u64,
    // Risk control
    /// Maximum unrealized loss on Leg1 before aborting
    pub max_leg1_loss: Decimal,
    /// If sum < this value, force-complete Leg2 even at timeout
    pub force_complete_threshold: Decimal,
    /// Minimum ask price to consider (filters out illiquid extreme prices)
    pub min_ask_price: Decimal,
    /// Minimum up_ask + down_ask to enter (filters out illiquid extreme-price pairs)
    pub min_entry_sum: Decimal,
    // Window filter
    /// Allowed window durations in seconds (e.g. [300, 900] for 5m + 15m).
    /// Empty = accept all durations.
    pub allowed_window_durations: Vec<u64>,
    /// Tolerance in seconds when matching window durations (default 30)
    pub window_duration_tolerance: u64,
    // Execution realism
    /// Minimum seconds between Leg1 fill and Leg2 fill (simulates CLOB latency)
    pub min_leg2_delay_secs: u64,
    /// Maximum trades per event window (prevents overtrading same window)
    pub max_trades_per_event: usize,
    // Vol model
    /// Drift estimate for log-normal model
    pub mu: f64,
    /// Volatility lookback window in seconds
    pub vol_lookback_secs: u64,
    /// Volatility floor to prevent overconfidence
    pub vol_floor: f64,
    /// Cooldown between entries on same symbol (seconds)
    pub cooldown_secs: u64,
}

impl Default for StaggeredArbBacktestConfig {
    fn default() -> Self {
        Self {
            symbols: vec!["BTCUSDT".to_string()],
            initial_capital: dec!(10000),
            shares_per_trade: 20,
            max_concurrent_positions: 5,
            direction_threshold: 0.03,
            max_initial_sum: dec!(1.10),
            min_profit_target: dec!(0.005),
            max_wait_secs: 180,
            max_wait_pct: 0.40,
            min_time_remaining_secs: 60,
            max_leg1_loss: dec!(0),
            force_complete_threshold: dec!(1.00),
            min_ask_price: dec!(0.05),
            min_entry_sum: dec!(0.70),
            allowed_window_durations: vec![300, 900],
            window_duration_tolerance: 30,
            min_leg2_delay_secs: 3,
            max_trades_per_event: 2,
            mu: 0.0,
            vol_lookback_secs: 300,
            vol_floor: 0.005,
            cooldown_secs: 5,
        }
    }
}

impl StaggeredArbBacktestConfig {
    pub fn with_symbols(symbols: Vec<String>) -> Self {
        Self {
            symbols,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StaggeredArbBacktestConfig;

    #[test]
    fn with_symbols_overrides_only_symbol_list() {
        let symbols = vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()];
        let config = StaggeredArbBacktestConfig::with_symbols(symbols.clone());

        assert_eq!(config.symbols, symbols);
        assert_eq!(config.shares_per_trade, 20);
        assert_eq!(config.max_concurrent_positions, 5);
        assert_eq!(config.allowed_window_durations, vec![300, 900]);
        assert_eq!(config.cooldown_secs, 5);
    }
}
