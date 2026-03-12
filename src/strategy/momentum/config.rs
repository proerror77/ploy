use super::*;
use serde::{Deserialize, Serialize};

/// Momentum strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumConfig {
    /// Minimum CEX price move to trigger (e.g., 0.003 = 0.3%)
    /// This is the BASE threshold, adjusted by volatility
    pub min_move_pct: Decimal,
    /// Maximum Polymarket odds for entry (e.g., 0.40 = 40¢)
    pub max_entry_price: Decimal,
    /// Minimum estimated edge to enter (e.g., 0.03 = 3%)
    pub min_edge: Decimal,
    /// Lookback window for momentum calculation (seconds)
    pub lookback_secs: u64,
    /// Use volatility-adjusted thresholds
    pub use_volatility_adjustment: bool,
    /// Baseline volatility for threshold adjustment (60s rolling std dev)
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
    /// Hold positions to resolution (don't exit early)
    pub hold_to_resolution: bool,
    /// Minimum time remaining to enter (seconds)
    pub min_time_remaining_secs: u64,
    /// Maximum time remaining to enter (seconds)
    pub max_time_remaining_secs: u64,
    /// Maximum total exposure across all symbols per 15-min window (USD)
    pub max_window_exposure_usd: Decimal,
    /// Only enter the highest edge signal per 15-min window
    pub best_edge_only: bool,
    /// Delay before selecting best edge (milliseconds)
    pub signal_collection_delay_ms: u64,
    /// Require all timeframes (10s, 30s, 60s) to agree on direction
    pub require_mtf_agreement: bool,
    /// Minimum OBI (Order Book Imbalance) for confirmation
    pub min_obi_confirmation: Decimal,
    /// Use K-line historical volatility instead of tick-based
    pub use_kline_volatility: bool,
    /// Time decay factor: reduce signal strength as event progresses
    pub time_decay_factor: Decimal,
    /// Consider price-to-beat in fair value calculation
    pub use_price_to_beat: bool,
    /// Dynamic position sizing based on signal confidence
    pub dynamic_position_sizing: bool,
    /// Minimum confidence for entry (0.0 - 1.0)
    pub min_confidence: f64,
    /// Enable Kelly-based position scaling.
    pub use_kelly_sizing: bool,
    /// Cap used to normalize Kelly sizing (e.g., 0.25 = quarter-Kelly cap).
    pub kelly_fraction_cap: Decimal,
    /// Require spot price direction to agree with VWAP.
    pub require_vwap_confirmation: bool,
    /// VWAP lookback window (seconds).
    pub vwap_lookback_secs: u64,
    /// Minimum deviation from VWAP required for confirmation (e.g., 0.001 = 0.1%).
    pub min_vwap_deviation: Decimal,
    /// When true, on_cex_update() uses estimate_probability() + FeeModel.
    pub directional_mode: bool,
    /// Volatility floor for probability model (directional mode only).
    pub directional_vol_floor: f64,
}

impl Default for MomentumConfig {
    fn default() -> Self {
        let mut baseline_volatility = HashMap::new();
        baseline_volatility.insert("BTCUSDT".into(), dec!(0.0005));
        baseline_volatility.insert("ETHUSDT".into(), dec!(0.0008));
        baseline_volatility.insert("SOLUSDT".into(), dec!(0.0015));
        baseline_volatility.insert("XRPUSDT".into(), dec!(0.0012));

        Self {
            min_move_pct: dec!(0.0005),
            max_entry_price: dec!(0.35),
            min_edge: dec!(0.03),
            lookback_secs: 5,
            use_volatility_adjustment: true,
            baseline_volatility,
            volatility_lookback_secs: 60,
            shares_per_trade: 100,
            max_positions: 3,
            cooldown_secs: 60,
            max_daily_trades: 20,
            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "XRPUSDT".into(),
            ],
            hold_to_resolution: true,
            min_time_remaining_secs: 60,
            max_time_remaining_secs: 300,
            max_window_exposure_usd: dec!(25),
            best_edge_only: true,
            signal_collection_delay_ms: 2000,
            require_mtf_agreement: true,
            min_obi_confirmation: dec!(0.05),
            use_kline_volatility: true,
            time_decay_factor: dec!(0.3),
            use_price_to_beat: true,
            dynamic_position_sizing: true,
            min_confidence: 0.5,
            use_kelly_sizing: true,
            kelly_fraction_cap: dec!(0.25),
            require_vwap_confirmation: false,
            vwap_lookback_secs: 60,
            min_vwap_deviation: dec!(0),
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
            take_profit_pct: dec!(0.20),
            stop_loss_pct: dec!(0.15),
            trailing_stop_pct: dec!(0.10),
            exit_before_resolution_secs: 30,
        }
    }
}

/// Trading direction (Up or Down)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

impl Direction {
    pub fn opposite(&self) -> Self {
        match self {
            Direction::Up => Direction::Down,
            Direction::Down => Direction::Up,
        }
    }
}

impl From<Direction> for Side {
    fn from(dir: Direction) -> Self {
        match dir {
            Direction::Up => Side::Up,
            Direction::Down => Side::Down,
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Up => write!(f, "UP"),
            Direction::Down => write!(f, "DOWN"),
        }
    }
}
