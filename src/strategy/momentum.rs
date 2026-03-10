//! Momentum strategy for Polymarket trading
//!
//! Implements the "gabagool22" style strategy:
//! 1. Monitor CEX (Binance) for BTC/ETH/SOL price movements
//! 2. When spot price moves significantly, Polymarket odds lag
//! 3. Enter the side that should win before odds adjust
//! 4. Exit via take-profit, stop-loss, trailing stop, or hold to resolution

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, error, info, trace, warn};

use crate::adapters::{
    ChainlinkPriceCache, ChainlinkUpdate, GammaEventInfo, PolymarketClient, PriceCache,
    PriceUpdate, QuoteCache, QuoteUpdate, SpotPrice,
};
use crate::config::RiskConfig;
use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::platform::CryptoDataPlaneHandle;
use crate::strategy::crypto::{
    horizon_for_series as crypto_horizon_for_series, known_binance_symbols,
    series_ids_for_symbol as crypto_series_ids_for_symbol,
};
use crate::strategy::dump_hedge::{DumpHedgeConfig, DumpHedgeEngine};
use crate::strategy::fee_model::FeeModel;
use crate::strategy::fund_manager::{FundManager, PositionSizeResult};
use crate::strategy::probability;
use crate::strategy::volatility::{EventTracker, VolatilityConfig, VolatilityDetector};
use crate::strategy::OrderExecutor;

mod matcher;
mod detector;

pub use detector::{MomentumDetector, MomentumSignal};
pub use matcher::{EventInfo, EventMatcher};

// ============================================================================
// Configuration
// ============================================================================

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

// ============================================================================
// Direction
// ============================================================================

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

// ============================================================================
// Position & Exit Manager
// ============================================================================

/// An open position
#[derive(Debug, Clone)]
pub struct Position {
    pub token_id: String,
    pub symbol: String,
    pub direction: Direction,
    pub entry_price: Decimal,
    pub entry_notional: Decimal,
    pub shares: u64,
    pub entry_time: DateTime<Utc>,
    pub highest_price: Decimal,
    pub event_end_time: DateTime<Utc>,
    pub event_slug: String,
    pub condition_id: String,
    /// P_hat at entry time (for probability-stop exit rule)
    pub entry_p_hat: Option<f64>,
    /// Chainlink open price (S0) at window start
    pub window_open_price: Option<Decimal>,
}

impl Position {
    /// Calculate current P&L percentage
    pub fn pnl_pct(&self, current_price: Decimal) -> Decimal {
        if self.entry_price.is_zero() {
            return Decimal::ZERO;
        }
        (current_price - self.entry_price) / self.entry_price
    }

    /// Update highest price seen (for trailing stop)
    pub fn update_high(&mut self, price: Decimal) {
        if price > self.highest_price {
            self.highest_price = price;
        }
    }

    /// Time remaining until event resolution
    pub fn time_to_resolution(&self) -> ChronoDuration {
        self.event_end_time - Utc::now()
    }
}

/// Reason for exiting a position
#[derive(Debug, Clone)]
pub enum ExitReason {
    TakeProfit {
        profit_pct: Decimal,
    },
    StopLoss {
        loss_pct: Decimal,
    },
    TrailingStop {
        high: Decimal,
        current: Decimal,
    },
    TimeExit,
    Manual,
    /// Probability model thesis invalidated (p_hat dropped below threshold)
    ProbabilityStop {
        entry_p_hat: f64,
        current_p_hat: f64,
    },
    /// Hard loss limit per trade
    HardStop {
        loss_usd: Decimal,
    },
}

impl std::fmt::Display for ExitReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExitReason::TakeProfit { profit_pct } => {
                write!(f, "TakeProfit({:.1}%)", profit_pct * dec!(100))
            }
            ExitReason::StopLoss { loss_pct } => {
                write!(f, "StopLoss({:.1}%)", loss_pct * dec!(100))
            }
            ExitReason::TrailingStop { high, current } => {
                write!(
                    f,
                    "TrailingStop(high={:.2}¢, cur={:.2}¢)",
                    high * dec!(100),
                    current * dec!(100)
                )
            }
            ExitReason::TimeExit => write!(f, "TimeExit"),
            ExitReason::Manual => write!(f, "Manual"),
            ExitReason::ProbabilityStop {
                entry_p_hat,
                current_p_hat,
            } => {
                write!(
                    f,
                    "ProbStop(entry={:.0}%→{:.0}%)",
                    entry_p_hat * 100.0,
                    current_p_hat * 100.0
                )
            }
            ExitReason::HardStop { loss_usd } => write!(f, "HardStop(${:.2})", loss_usd),
        }
    }
}

/// Manages position exits
pub struct ExitManager {
    config: ExitConfig,
}

impl ExitManager {
    pub fn new(config: ExitConfig) -> Self {
        Self { config }
    }

    /// Check if position should be exited
    pub fn check_exit(&self, pos: &Position, current_bid: Decimal) -> Option<ExitReason> {
        let pnl_pct = pos.pnl_pct(current_bid);

        // 1. Take Profit
        if pnl_pct >= self.config.take_profit_pct {
            return Some(ExitReason::TakeProfit {
                profit_pct: pnl_pct,
            });
        }

        // 2. Stop Loss
        if pnl_pct <= -self.config.stop_loss_pct {
            return Some(ExitReason::StopLoss { loss_pct: -pnl_pct });
        }

        // 3. Trailing Stop (only if we've been profitable)
        if pos.highest_price > pos.entry_price && current_bid < pos.highest_price {
            let drop_from_high = (pos.highest_price - current_bid) / pos.highest_price;
            if drop_from_high >= self.config.trailing_stop_pct {
                return Some(ExitReason::TrailingStop {
                    high: pos.highest_price,
                    current: current_bid,
                });
            }
        }

        // 4. Time-based exit before resolution
        let time_to_resolution = pos.time_to_resolution();
        if time_to_resolution.num_seconds() < self.config.exit_before_resolution_secs as i64 {
            return Some(ExitReason::TimeExit);
        }

        None
    }
}

// ============================================================================
// Momentum Engine
// ============================================================================

/// Daily trade counter for rate limiting
#[derive(Debug, Default)]
struct DailyTradeCounter {
    count: u32,
    reset_date: Option<chrono::NaiveDate>,
}

impl DailyTradeCounter {
    fn increment(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count += 1;
        self.count
    }

    fn current(&mut self) -> u32 {
        let today = Utc::now().date_naive();
        if self.reset_date != Some(today) {
            self.count = 0;
            self.reset_date = Some(today);
        }
        self.count
    }
}

/// Pending signal for best-edge selection
#[derive(Debug, Clone)]
struct PendingSignal {
    signal: MomentumSignal,
    event: EventInfo,
    edge: Decimal,
    cost_usd: Decimal,
    timestamp: DateTime<Utc>,
}

/// Window risk tracker for cross-symbol exposure limits
/// Tracks exposure per 15-min window (grouped by event end time)
#[derive(Debug, Default)]
struct WindowRiskTracker {
    /// Exposure by window ID (event end time as string)
    window_exposure: HashMap<String, Decimal>,
    /// Pending signals per window (for best-edge selection)
    pending_signals: HashMap<String, Vec<PendingSignal>>,
    /// Windows that have been executed (to prevent duplicates)
    executed_windows: HashMap<String, bool>,
}

impl WindowRiskTracker {
    /// Get window ID from event end time (rounded to 15-min)
    fn window_id(event_end: &DateTime<Utc>) -> String {
        // Format: YYYY-MM-DD HH:MM where MM is rounded to 15-min boundary
        let ts = event_end.timestamp();
        let rounded = (ts / 900) * 900; // Round down to 15-min boundary
        DateTime::from_timestamp(rounded, 0)
            .unwrap_or(*event_end)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    }

    /// Check if window already has an executed trade
    fn has_executed(&self, window_id: &str) -> bool {
        self.executed_windows
            .get(window_id)
            .copied()
            .unwrap_or(false)
    }

    /// Mark window as executed
    fn mark_executed(&mut self, window_id: &str) {
        self.executed_windows.insert(window_id.to_string(), true);
    }

    /// Get current exposure for a window
    fn get_exposure(&self, window_id: &str) -> Decimal {
        self.window_exposure
            .get(window_id)
            .copied()
            .unwrap_or(Decimal::ZERO)
    }

    /// Add exposure to a window
    fn add_exposure(&mut self, window_id: &str, amount: Decimal) {
        let current = self.get_exposure(window_id);
        self.window_exposure
            .insert(window_id.to_string(), current + amount);
    }

    /// Add pending signal for a window
    fn add_pending_signal(&mut self, window_id: &str, signal: PendingSignal) {
        self.pending_signals
            .entry(window_id.to_string())
            .or_default()
            .push(signal);
    }

    /// Get best signal for a window (highest edge)
    fn get_best_signal(&self, window_id: &str) -> Option<PendingSignal> {
        self.pending_signals
            .get(window_id)
            .and_then(|signals| signals.iter().max_by(|a, b| a.edge.cmp(&b.edge)).cloned())
    }

    /// Clear pending signals for a window
    fn clear_pending(&mut self, window_id: &str) {
        self.pending_signals.remove(window_id);
    }

    /// Check if there are pending signals ready for execution (past delay threshold)
    fn get_ready_windows(&self, delay_ms: u64) -> Vec<String> {
        let now = Utc::now();
        let threshold = ChronoDuration::milliseconds(delay_ms as i64);

        self.pending_signals
            .keys()
            .filter(|window_id| {
                // Check if window has signals and oldest is past threshold
                if let Some(signals) = self.pending_signals.get(*window_id) {
                    if let Some(oldest) = signals.iter().min_by_key(|s| s.timestamp) {
                        return now.signed_duration_since(oldest.timestamp) >= threshold;
                    }
                }
                false
            })
            .cloned()
            .collect()
    }

    /// Cleanup old windows (older than 30 min)
    fn cleanup_old(&mut self) {
        let now = Utc::now();
        let cutoff = now - ChronoDuration::minutes(30);
        let cutoff_str = Self::window_id(&cutoff);

        self.window_exposure.retain(|k, _| k >= &cutoff_str);
        self.executed_windows.retain(|k, _| k >= &cutoff_str);
        self.pending_signals.retain(|k, _| k >= &cutoff_str);
    }
}

/// Main engine orchestrating the momentum strategy
pub struct MomentumEngine {
    config: MomentumConfig,
    #[allow(dead_code)]
    exit_config: ExitConfig,
    detector: MomentumDetector,
    exit_manager: ExitManager,
    event_matcher: EventMatcher,
    executor: OrderExecutor,
    positions: Arc<RwLock<HashMap<String, Position>>>,
    last_trade_time: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    daily_trades: Arc<RwLock<DailyTradeCounter>>,
    dry_run: bool,
    // Volatility-based event tracking
    volatility_detector: VolatilityDetector,
    event_tracker: Arc<RwLock<EventTracker>>,
    // Fund management
    fund_manager: Option<Arc<FundManager>>,
    // Auto-claimer for winning positions
    #[cfg(feature = "claimer_daemon")]
    claimer: Option<Arc<super::claimer::AutoClaimer>>,
    // Trade logger for persistent records
    trade_logger: Option<Arc<super::trade_logger::TradeLogger>>,
    // Window risk tracker for cross-symbol exposure limits
    window_tracker: Arc<RwLock<WindowRiskTracker>>,
    // Binance LOB cache for OBI signals
    lob_cache: Option<Arc<crate::collector::LobCache>>,
    // K-line client for historical volatility
    kline_client: Option<Arc<crate::collector::BinanceKlineClient>>,
    // Dump & Hedge strategy engine
    dump_hedge: Option<Arc<DumpHedgeEngine>>,
    // Serialize entry path to avoid duplicate orders for the same event under concurrent updates.
    entry_mutex: Arc<Mutex<()>>,
    // === Directional prediction infrastructure ===
    // Dynamic fee model for cost-aware entry
    fee_model: FeeModel,
    // Chainlink price cache for ground truth oracle prices
    chainlink_cache: Option<ChainlinkPriceCache>,
    // Entry threshold for EV_net (directional mode)
    entry_threshold: f64,
}

impl MomentumEngine {
    /// Create a new momentum engine
    pub fn new(
        config: MomentumConfig,
        exit_config: ExitConfig,
        client: PolymarketClient,
        executor: OrderExecutor,
        dry_run: bool,
    ) -> Self {
        let detector = MomentumDetector::new(config.clone());
        let exit_manager = ExitManager::new(exit_config.clone());
        let event_matcher = EventMatcher::new(client);

        // Initialize volatility detector with config matching momentum settings
        let volatility_config = VolatilityConfig {
            max_entry_price: config.max_entry_price,
            min_edge: config.min_edge,
            min_deviation_pct: config.min_move_pct, // Use same threshold
            shares_per_trade: config.shares_per_trade,
            min_time_remaining_secs: config.min_time_remaining_secs,
            max_time_remaining_secs: config.max_time_remaining_secs,
            ..Default::default()
        };
        let volatility_detector = VolatilityDetector::new(volatility_config);
        let event_tracker = EventTracker::new(20); // Keep 20 historical events

        Self {
            config,
            exit_config,
            detector,
            exit_manager,
            event_matcher,
            executor,
            positions: Arc::new(RwLock::new(HashMap::new())),
            last_trade_time: Arc::new(RwLock::new(HashMap::new())),
            daily_trades: Arc::new(RwLock::new(DailyTradeCounter::default())),
            dry_run,
            volatility_detector,
            event_tracker: Arc::new(RwLock::new(event_tracker)),
            fund_manager: None,
            #[cfg(feature = "claimer_daemon")]
            claimer: None,
            trade_logger: None,
            window_tracker: Arc::new(RwLock::new(WindowRiskTracker::default())),
            lob_cache: None,
            kline_client: None,
            dump_hedge: None,
            entry_mutex: Arc::new(Mutex::new(())),
            fee_model: FeeModel::crypto(),
            chainlink_cache: None,
            entry_threshold: 0.08,
        }
    }

    /// Set Binance LOB cache for OBI signals
    pub fn with_lob_cache(mut self, cache: crate::collector::LobCache) -> Self {
        self.lob_cache = Some(Arc::new(cache));
        self
    }

    /// Set K-line client for historical volatility
    pub fn with_kline_client(mut self, client: crate::collector::BinanceKlineClient) -> Self {
        self.kline_client = Some(Arc::new(client));
        self
    }

    /// Enable Dump & Hedge strategy
    pub fn with_dump_hedge(mut self, config: DumpHedgeConfig) -> Self {
        self.dump_hedge = Some(Arc::new(DumpHedgeEngine::new(config)));
        self
    }

    /// Set dynamic fee model (default: crypto parabolic)
    pub fn with_fee_model(mut self, model: FeeModel) -> Self {
        self.fee_model = model;
        self
    }

    /// Set Chainlink price cache for directional prediction
    pub fn with_chainlink_cache(mut self, cache: ChainlinkPriceCache) -> Self {
        self.chainlink_cache = Some(cache);
        self
    }

    /// Set EV_net entry threshold for directional mode (default: 0.08)
    pub fn with_entry_threshold(mut self, threshold: f64) -> Self {
        self.entry_threshold = threshold;
        self
    }

    /// Create a new momentum engine with fund management
    pub fn new_with_fund_manager(
        config: MomentumConfig,
        exit_config: ExitConfig,
        client: PolymarketClient,
        executor: OrderExecutor,
        risk_config: RiskConfig,
        dry_run: bool,
    ) -> Self {
        let fund_manager = FundManager::new(client.clone(), risk_config);
        let mut engine = Self::new(config, exit_config, client, executor, dry_run);
        engine.fund_manager = Some(Arc::new(fund_manager));
        engine
    }

    /// Set fund manager
    pub fn with_fund_manager(mut self, fund_manager: FundManager) -> Self {
        self.fund_manager = Some(Arc::new(fund_manager));
        self
    }

    /// Set auto-claimer for winning positions
    #[cfg(feature = "claimer_daemon")]
    pub fn with_claimer(mut self, claimer: super::claimer::AutoClaimer) -> Self {
        self.claimer = Some(Arc::new(claimer));
        self
    }

    /// Set trade logger for persistent records
    pub fn with_trade_logger(mut self, logger: super::trade_logger::TradeLogger) -> Self {
        self.trade_logger = Some(Arc::new(logger));
        self
    }

    /// Get trade logger reference
    pub fn trade_logger(&self) -> Option<&Arc<super::trade_logger::TradeLogger>> {
        self.trade_logger.as_ref()
    }

    /// Check if daily trade limit reached
    async fn daily_limit_reached(&self) -> bool {
        if self.config.max_daily_trades == 0 {
            return false; // No limit
        }
        let mut counter = self.daily_trades.write().await;
        counter.current() >= self.config.max_daily_trades
    }

    /// Record a trade and return new count
    async fn record_trade(&self) -> u32 {
        let mut counter = self.daily_trades.write().await;
        counter.increment()
    }

    /// Estimate signal win probability from PM entry price + model edge.
    fn estimated_win_probability(&self, signal: &MomentumSignal) -> Decimal {
        (signal.pm_price + signal.edge)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    /// Binary-outcome Kelly fraction for contracts that pay $1 on win and cost `price`.
    /// f* = (p - price) / (1 - price), clamped to [0, 1].
    fn signal_kelly_fraction(&self, signal: &MomentumSignal) -> Decimal {
        if signal.pm_price <= Decimal::ZERO || signal.pm_price >= Decimal::ONE {
            return Decimal::ZERO;
        }

        let p = self.estimated_win_probability(signal);
        let denom = Decimal::ONE - signal.pm_price;
        if denom <= Decimal::ZERO {
            return Decimal::ZERO;
        }

        ((p - signal.pm_price) / denom)
            .max(Decimal::ZERO)
            .min(Decimal::ONE)
    }

    /// Apply confidence/Kelly scaling on top of base shares from fund manager.
    fn apply_signal_position_sizing(&self, base_shares: u64, signal: &MomentumSignal) -> u64 {
        if base_shares == 0 {
            return 0;
        }

        let mut multiplier = Decimal::ONE;

        if self.config.dynamic_position_sizing {
            let conf = Decimal::from_f64(signal.confidence.clamp(0.0, 1.0)).unwrap_or(Decimal::ONE);
            multiplier *= conf;
        }

        if self.config.use_kelly_sizing {
            let kelly = self.signal_kelly_fraction(signal);
            let cap = self.config.kelly_fraction_cap.max(dec!(0.0001));
            let normalized = (kelly / cap).min(Decimal::ONE);
            multiplier *= normalized;
        }

        let scaled = (Decimal::from(base_shares) * multiplier)
            .floor()
            .to_u64()
            .unwrap_or(0);

        if scaled == 0 {
            debug!(
                "Position size scaled to 0 (base_shares={}, multiplier={:.4})",
                base_shares, multiplier
            );
        }

        scaled
    }

    /// Get event matcher reference
    pub fn event_matcher(&self) -> &EventMatcher {
        &self.event_matcher
    }

    /// Get positions count
    pub async fn positions_count(&self) -> usize {
        self.positions.read().await.len()
    }

    /// Check for resolved positions and handle them
    /// Returns (won_count, lost_count, total_payout)
    pub async fn check_resolved_positions(&self) -> (u32, u32, Decimal) {
        let now = Utc::now();
        let mut won_count = 0u32;
        let mut lost_count = 0u32;
        let mut total_payout = Decimal::ZERO;

        // Find positions that have passed their end time
        let resolved_symbols: Vec<String> = {
            let positions = self.positions.read().await;
            positions
                .iter()
                .filter(|(_, pos)| pos.event_end_time < now)
                .map(|(symbol, _)| symbol.clone())
                .collect()
        };

        if resolved_symbols.is_empty() {
            return (0, 0, Decimal::ZERO);
        }

        info!(
            "🔍 Checking {} resolved positions...",
            resolved_symbols.len()
        );

        for symbol in resolved_symbols {
            // Get position details
            let pos_opt = {
                let positions = self.positions.read().await;
                positions.get(&symbol).cloned()
            };

            let pos = match pos_opt {
                Some(p) => p,
                None => continue,
            };

            // Check market status via API
            let market_result = self
                .event_matcher
                .client()
                .get_market(&pos.condition_id)
                .await;

            match market_result {
                Ok(market) => {
                    if !market.closed {
                        // Market not yet closed, wait
                        debug!("{} market not closed yet, waiting...", symbol);
                        continue;
                    }

                    if !self.market_is_settled(&market) {
                        debug!(
                            "{} market closed but not settled yet (outcome prices not 1/0), waiting...",
                            symbol
                        );
                        continue;
                    }

                    // Determine win/loss by checking token prices
                    // Winner token price = 1.0, loser = 0.0
                    let won = self.check_if_won(&pos, &market);

                    if won {
                        let payout = Decimal::from(pos.shares); // Each winning share = $1
                        let profit = payout - (pos.entry_price * Decimal::from(pos.shares));

                        info!(
                            "🎉 {} WON! {} {} | {} shares @ {:.2}¢ → ${:.2} payout (+${:.2} profit)",
                            symbol,
                            pos.direction,
                            pos.event_slug,
                            pos.shares,
                            pos.entry_price * dec!(100),
                            payout,
                            profit
                        );

                        won_count += 1;
                        total_payout += payout;

                        #[cfg(feature = "claimer_daemon")]
                        {
                            // Trigger claimer to redeem winning position
                            if let Some(ref claimer) = self.claimer {
                                info!(
                                    "📋 Triggering claimer for {}: condition_id={}, shares={}",
                                    symbol,
                                    &pos.condition_id[..16.min(pos.condition_id.len())],
                                    pos.shares
                                );
                                match claimer.check_and_claim().await {
                                    Ok(results) => {
                                        for result in results {
                                            if result.success {
                                                info!(
                                                    "✅ Claimed ${:.2} from {}: tx={}",
                                                    result.amount_claimed,
                                                    &result.condition_id
                                                        [..16.min(result.condition_id.len())],
                                                    result.tx_hash
                                                );
                                            } else if let Some(err) = result.error {
                                                warn!(
                                                    "❌ Failed to claim {}: {}",
                                                    result.condition_id, err
                                                );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Failed to trigger claimer: {}", e);
                                    }
                                }
                            } else {
                                // No claimer configured - just log
                                info!(
                                    "📋 Position {} needs claiming (no claimer configured): condition_id={}, shares={}",
                                    symbol,
                                    &pos.condition_id[..16.min(pos.condition_id.len())],
                                    pos.shares
                                );
                            }
                        }

                        #[cfg(not(feature = "claimer_daemon"))]
                        {
                            info!(
                                "📋 Position {} needs claiming (claimer feature disabled): condition_id={}, shares={}",
                                symbol,
                                &pos.condition_id[..16.min(pos.condition_id.len())],
                                pos.shares
                            );
                        }
                    } else {
                        let loss = pos.entry_price * Decimal::from(pos.shares);
                        info!(
                            "❌ {} LOST: {} {} | {} shares @ {:.2}¢ → -${:.2}",
                            symbol,
                            pos.direction,
                            pos.event_slug,
                            pos.shares,
                            pos.entry_price * dec!(100),
                            loss
                        );
                        lost_count += 1;
                    }

                    // Log trade resolution
                    if let Some(ref logger) = self.trade_logger {
                        logger.record_resolution(&pos.condition_id, won).await;
                    }

                    // Remove from positions
                    {
                        let mut positions = self.positions.write().await;
                        positions.remove(&symbol);
                    }

                    // Update fund manager
                    if let Some(ref fm) = self.fund_manager {
                        let released_notional = if pos.entry_notional > Decimal::ZERO {
                            pos.entry_notional
                        } else {
                            pos.entry_price * Decimal::from(pos.shares)
                        };
                        fm.record_position_closed_with_amount(
                            &pos.condition_id,
                            &pos.symbol,
                            released_notional,
                        )
                        .await;
                    }
                }
                Err(e) => {
                    warn!("Failed to get market status for {}: {}", symbol, e);
                }
            }
        }

        if won_count > 0 || lost_count > 0 {
            info!(
                "📊 Resolution summary: {} won, {} lost, ${:.2} payout pending claim",
                won_count, lost_count, total_payout
            );
        }

        (won_count, lost_count, total_payout)
    }

    fn market_is_settled(&self, market: &crate::adapters::MarketResponse) -> bool {
        use rust_decimal::Decimal;
        use rust_decimal_macros::dec;

        // Avoid prematurely treating a market as resolved just because prices move close to 1/0.
        // We only accept settlement once the market is actually closed.
        if !market.closed {
            return false;
        }

        let mut prices = Vec::new();
        for t in &market.tokens {
            let Some(ref price_str) = t.price else {
                continue;
            };
            if let Ok(p) = price_str.parse::<Decimal>() {
                prices.push(p);
            }
        }

        if prices.is_empty() {
            return false;
        }

        // Official settlement: exactly one winner ~1, all losers ~0.
        let winners = prices.iter().filter(|p| **p >= dec!(0.99)).count();
        let losers = prices.iter().filter(|p| **p <= dec!(0.01)).count();
        winners == 1 && losers == prices.len().saturating_sub(1)
    }

    /// Check if we won based on market outcome prices
    fn check_if_won(&self, pos: &Position, market: &crate::adapters::MarketResponse) -> bool {
        // Find our token in the market tokens
        for token in &market.tokens {
            if token.token_id == pos.token_id {
                // Parse the price - winner has price = 1.0
                if let Some(ref price_str) = token.price {
                    if let Ok(price) = price_str.parse::<f64>() {
                        return price >= 0.99; // Winner = 1.0, Loser = 0.0
                    }
                }
            }
        }

        // Fallback: if we bought Up and price went up, we likely won
        // This is a heuristic in case outcome_prices not available
        warn!(
            "Could not determine outcome from market data for {}, using heuristic",
            pos.symbol
        );
        false
    }

    /// Run the momentum strategy
    pub async fn run(
        &self,
        market_data: &CryptoDataPlaneHandle,
        mut chainlink_rx: Option<broadcast::Receiver<ChainlinkUpdate>>,
        chainlink_cache: Option<&ChainlinkPriceCache>,
    ) -> Result<()> {
        info!("Starting momentum engine (dry_run={})", self.dry_run);
        let mut binance_rx = market_data.subscribe_prices();
        let mut pm_rx = market_data.subscribe_quotes();
        let binance_cache = market_data.price_cache();
        let pm_cache = market_data.quote_cache();

        // Log mode-specific configuration
        if self.config.hold_to_resolution {
            info!("=== CRYINGLITTLEBABY CONFIRMATORY MODE ===");
            info!(
                "• Entry window: {}-{}s before resolution",
                self.config.min_time_remaining_secs, self.config.max_time_remaining_secs
            );
            info!("• Hold to resolution: YES (collect $1)");
            info!(
                "• Min CEX move: {:.2}%, Max entry: {:.0}¢",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
        } else {
            info!("=== PREDICTIVE MODE (early entry) ===");
            info!(
                "Config: min_move={:.2}%, max_entry={:.0}¢, min_edge={:.1}%",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100),
                self.config.min_edge * dec!(100)
            );
        }

        // Refresh events initially
        if let Err(e) = self.event_matcher.refresh().await {
            error!("Failed to refresh events: {}", e);
        }

        // Periodic event refresh
        let event_matcher = &self.event_matcher;
        let refresh_interval = tokio::time::interval(Duration::from_secs(60));
        tokio::pin!(refresh_interval);

        // Resolution check interval (every 30 seconds)
        let resolution_interval = tokio::time::interval(Duration::from_secs(30));
        tokio::pin!(resolution_interval);

        // Pending signal processing interval (every 500ms when best_edge_only is enabled)
        let signal_process_interval = tokio::time::interval(Duration::from_millis(500));
        tokio::pin!(signal_process_interval);

        // Log cross-symbol risk settings
        if self.config.best_edge_only {
            info!("=== CROSS-SYMBOL RISK CONTROL ===");
            info!("• Best edge only: YES (queue signals, select highest edge)");
            info!(
                "• Signal collection delay: {}ms",
                self.config.signal_collection_delay_ms
            );
            info!(
                "• Max window exposure: ${:.2}",
                self.config.max_window_exposure_usd
            );
        }

        let has_chainlink = chainlink_rx.is_some();
        if has_chainlink {
            info!("=== DIRECTIONAL PREDICTION MODE ===");
            info!("• Ground truth: Chainlink RTDS (not Binance)");
            info!("• Fee model: parabolic (crypto, fee_rate=0.25, exp=2)");
            info!(
                "• Entry threshold: EV_net >= {:.1}%",
                self.entry_threshold * 100.0
            );
        }

        if self.config.directional_mode {
            info!("=== DIRECTIONAL MODE (BINANCE AS ORACLE) ===");
            info!("• Ground truth: Binance spot price (Chainlink proxy)");
            info!("• Fee model: parabolic (crypto, fee_rate=0.25, exp=2)");
            info!(
                "• Entry threshold: EV_net >= {:.1}%",
                self.entry_threshold * 100.0
            );
            info!("• Vol floor: {:.4}", self.config.directional_vol_floor);
            info!("• Symbols: {:?}", self.config.symbols);
        }

        loop {
            tokio::select! {
                // PRIMARY (when Chainlink active): Oracle price → probability → entry
                Ok(cl_update) = async {
                    match chainlink_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    if let Some(cl_cache) = chainlink_cache {
                        if let Err(e) = self.on_chainlink_update(&cl_update, cl_cache, &binance_cache, &pm_cache).await {
                            error!("Error processing Chainlink update: {}", e);
                        }
                    }
                }

                // CEX price update - entry signals (fallback when no Chainlink)
                Ok(price_update) = binance_rx.recv() => {
                    // When Chainlink is active, Binance is features-only (no direct entry)
                    if !has_chainlink {
                        if let Err(e) = self.on_cex_update(&price_update, &binance_cache, &pm_cache).await {
                            error!("Error processing CEX update: {}", e);
                        }
                    }
                }

                // Polymarket quote update - check exit conditions
                Ok(quote_update) = pm_rx.recv() => {
                    if let Err(e) = self.on_pm_update(&quote_update).await {
                        error!("Error processing PM update: {}", e);
                    }
                }

                // Periodic event refresh
                _ = refresh_interval.tick() => {
                    if let Err(e) = event_matcher.refresh().await {
                        warn!("Failed to refresh events: {}", e);
                    }
                }

                // Check for resolved positions (hold_to_resolution mode)
                _ = resolution_interval.tick() => {
                    if self.config.hold_to_resolution {
                        let (_won, _lost, _payout) = self.check_resolved_positions().await;
                    }
                }

                // Process pending signals (best_edge_only mode)
                _ = signal_process_interval.tick() => {
                    if self.config.best_edge_only {
                        if let Err(e) = self.process_pending_signals().await {
                            error!("Error processing pending signals: {}", e);
                        }
                    }
                }
            }
        }
    }

    /// Handle CEX price update - check for entry signals
    async fn on_cex_update(
        &self,
        update: &PriceUpdate,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        let symbol = &update.symbol;

        // Check if we're tracking this symbol
        if !self.config.symbols.contains(symbol) {
            return Ok(());
        }

        // Get spot price with history
        let spot = match binance_cache.get(symbol).await {
            Some(s) => s,
            None => return Ok(()),
        };

        // Find matching event using appropriate timing mode
        // CRYINGLITTLEBABY: prefer events CLOSE to ending (1-5 min left)
        // Predictive: prefer events with more time remaining
        let event = if self.config.hold_to_resolution {
            // Confirmatory mode: find events within 1-5 min window
            match self
                .event_matcher
                .find_event_with_timing(
                    symbol,
                    self.config.min_time_remaining_secs,
                    self.config.max_time_remaining_secs as i64,
                    true, // prefer_close_to_end
                )
                .await
            {
                Some(e) => e,
                None => {
                    debug!(
                        "{} no event in confirmatory window ({}-{}s)",
                        symbol,
                        self.config.min_time_remaining_secs,
                        self.config.max_time_remaining_secs
                    );
                    return Ok(());
                }
            }
        } else {
            // Predictive mode: find events with more time remaining
            match self.event_matcher.find_event(symbol).await {
                Some(e) => e,
                None => {
                    debug!("No active event for {}", symbol);
                    return Ok(());
                }
            }
        };

        // Log timing info in confirmatory mode
        if self.config.hold_to_resolution {
            let remaining = event.time_remaining().num_seconds();
            debug!(
                "{} found event {} with {}s remaining (confirmatory mode)",
                symbol, event.title, remaining
            );
        }

        // Track event start price for volatility detection
        {
            let mut tracker = self.event_tracker.write().await;
            // Start or update event tracking
            if !tracker.has_active_event(&event.condition_id) {
                // New event - record start price
                tracker.start_event(
                    symbol.clone(),
                    event.condition_id.clone(),
                    event.end_time,
                    spot.price,
                );
                info!(
                    "📊 {} new event {} started at {:.2}, ends {}",
                    symbol,
                    &event.condition_id[..8],
                    spot.price,
                    event.end_time.format("%H:%M:%S")
                );
            } else {
                // Update existing event with current price
                tracker.update_price_by_event_id(&event.condition_id, spot.price);
            }
        }

        // Get PM quotes for this event
        let (up_ask, down_ask) = self.get_pm_prices(pm_cache, &event).await;

        // === DIRECTIONAL MODE: Binance as oracle, probability model entry ===
        if self.config.directional_mode {
            return self
                .directional_entry_from_binance(symbol, &spot, &event, up_ask, down_ask)
                .await;
        }

        // === MOMENTUM/VOLATILITY MODE (original path) ===
        // Check for momentum signal (CEX momentum-based)
        if let Some(signal) = self.detector.check(symbol, &spot, up_ask, down_ask) {
            self.maybe_enter(signal, &event).await?;
        }

        // Also check for volatility signal (deviation from start price)
        {
            // Get OBI from Binance LOB cache if available
            let obi = if let Some(ref lob) = self.lob_cache {
                lob.get_obi(symbol, 5).await // Use top 5 levels
            } else {
                None
            };

            let tracker = self.event_tracker.read().await;
            if let Some(vol_signal) = self.volatility_detector.check_signal(
                symbol,
                &event.condition_id,
                &tracker,
                up_ask,
                down_ask,
                obi,
                event.price_to_beat, // Pass price_to_beat from EventInfo
            ) {
                // Convert volatility signal to momentum signal for unified execution
                let momentum_signal = MomentumSignal {
                    symbol: symbol.clone(),
                    direction: match vol_signal.side {
                        Side::Up => Direction::Up,
                        Side::Down => Direction::Down,
                    },
                    cex_move_pct: vol_signal.deviation_pct,
                    pm_price: vol_signal.entry_price,
                    edge: vol_signal.edge,
                    confidence: vol_signal.confidence,
                    timestamp: Utc::now(),
                };
                info!(
                    "📈 {} VOLATILITY signal: {} deviation={:.3}% fair={:.2}¢ edge={:.1}%",
                    symbol,
                    vol_signal.side,
                    vol_signal.deviation_pct * dec!(100),
                    vol_signal.fair_value * dec!(100),
                    vol_signal.edge * dec!(100)
                );
                self.maybe_enter(momentum_signal, &event).await?;
            }
        }

        Ok(())
    }

    /// Directional mode entry using Binance as oracle (same logic as Chainlink path).
    ///
    /// Uses estimate_probability() + FeeModel to compute EV_net and enter
    /// when edge exceeds threshold. Binance spot price proxies Chainlink.
    async fn directional_entry_from_binance(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        event: &EventInfo,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Result<()> {
        let time_remaining = event.time_remaining().num_seconds() as f64;
        if time_remaining <= 0.0 {
            return Ok(());
        }

        // Get S0 from event tracker or price_to_beat
        let s0 = {
            let tracker = self.event_tracker.read().await;
            if let Some(record) = tracker.get_event(&event.condition_id) {
                record.start_price
            } else if let Some(ptb) = event.price_to_beat {
                ptb
            } else {
                return Ok(());
            }
        };

        let st = spot.price;

        // Compute realized vol from Binance history (scaled to period-level)
        let vol_floor = self.config.directional_vol_floor;
        let sigma = spot
            .volatility(300)
            .and_then(|v| v.to_f64())
            .map(|tick_vol| {
                if tick_vol > 0.0 {
                    let n_ticks = spot.history_len().min(5000) as f64;
                    (tick_vol * n_ticks.sqrt()).max(vol_floor)
                } else {
                    vol_floor
                }
            })
            .unwrap_or(vol_floor);

        // Estimate probability using log-normal model
        let p_hat = probability::estimate_probability(s0, st, sigma, time_remaining, 0.0);

        // Determine direction and get market ask price
        let (direction, market_ask) = if p_hat > 0.5 {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return Ok(()),
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return Ok(()),
            }
        };
        let effective_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };

        // Price bounds: skip extremes (too cheap = bad risk/reward, too expensive = low edge)
        if market_ask > self.config.max_entry_price {
            return Ok(());
        }
        if market_ask < dec!(0.10) {
            trace!(
                "Skipping {} {} — ask {:.2}¢ below 10¢ floor",
                symbol,
                direction,
                market_ask * dec!(100)
            );
            return Ok(());
        }

        // All-in cost via FeeModel (corrected: fee_per_share = price × effective_rate)
        let fee_model = FeeModel::crypto();
        let effective_rate = fee_model.effective_rate(market_ask);
        let fee_per_share = market_ask * effective_rate;
        let spread_cost = dec!(0.01); // Conservative 1¢ spread estimate
        let market_ask_f64 = market_ask.to_f64().unwrap_or(0.5);
        let cost_total_f64 =
            fee_per_share.to_f64().unwrap_or(0.01) + spread_cost.to_f64().unwrap_or(0.01);

        // EV_net check
        let ev_net = effective_p - market_ask_f64 - cost_total_f64;

        trace!(
            "🎯 {} {} p_hat={:.3} eff_p={:.3} ask={:.3} cost={:.4} ev_net={:.4} σ={:.5}",
            symbol,
            direction,
            p_hat,
            effective_p,
            market_ask_f64,
            cost_total_f64,
            ev_net,
            sigma
        );

        if ev_net < self.entry_threshold {
            return Ok(());
        }

        info!(
            "🎯 DIRECTIONAL ENTRY: {} {} p_hat={:.1}% ev_net={:.1}% ask={:.1}¢ σ={:.4}",
            symbol,
            direction,
            effective_p * 100.0,
            ev_net * 100.0,
            market_ask_f64 * 100.0,
            sigma,
        );

        // Create signal and enter via existing maybe_enter path
        let cex_move_pct = Decimal::try_from((st - s0) / s0).unwrap_or(Decimal::ZERO);
        let edge = Decimal::try_from(ev_net).unwrap_or(Decimal::ZERO);
        let signal = MomentumSignal {
            symbol: symbol.to_string(),
            direction,
            cex_move_pct,
            pm_price: market_ask,
            edge,
            confidence: effective_p,
            timestamp: Utc::now(),
        };

        self.maybe_enter(signal, event).await?;

        // Update position with p_hat and S0
        {
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions
                .values_mut()
                .find(|p| p.condition_id == event.condition_id)
            {
                pos.entry_p_hat = Some(p_hat);
                pos.window_open_price = Some(s0);
            }
        }

        Ok(())
    }

    /// Get Polymarket prices for an event
    async fn get_pm_prices(
        &self,
        pm_cache: &QuoteCache,
        event: &EventInfo,
    ) -> (Option<Decimal>, Option<Decimal>) {
        let up_quote = pm_cache.get(&event.up_token_id);
        let down_quote = pm_cache.get(&event.down_token_id);

        let up_ask = up_quote.and_then(|q| q.best_ask);
        let down_ask = down_quote.and_then(|q| q.best_ask);

        (up_ask, down_ask)
    }

    /// Handle Chainlink price update — directional probability-based entry
    ///
    /// This is the PRIMARY signal path for directional prediction mode.
    /// Uses the log-normal probability model to estimate P(Up) and checks
    /// EV_net = effective_p - market_ask - all_in_cost >= threshold.
    async fn on_chainlink_update(
        &self,
        update: &ChainlinkUpdate,
        chainlink_cache: &ChainlinkPriceCache,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        // Map Chainlink symbol to Binance symbol for event matching
        let binance_symbol =
            match crate::adapters::chainlink_rtds::to_binance_symbol(&update.symbol) {
                Some(s) => s.to_string(),
                None => return Ok(()),
            };

        if !self.config.symbols.contains(&binance_symbol) {
            return Ok(());
        }

        // Find matching event
        let event = match self
            .event_matcher
            .find_event_with_timing(
                &binance_symbol,
                self.config.min_time_remaining_secs,
                self.config.max_time_remaining_secs as i64,
                true,
            )
            .await
        {
            Some(e) => e,
            None => return Ok(()),
        };

        let time_remaining = event.time_remaining().num_seconds() as f64;
        if time_remaining <= 0.0 {
            return Ok(());
        }

        // Get Chainlink spot with history
        let cl_spot = match chainlink_cache.get(&update.symbol).await {
            Some(s) => s,
            None => return Ok(()),
        };

        // Get window open price (S0) from event tracker or price_to_beat
        let s0 = {
            let tracker = self.event_tracker.read().await;
            if let Some(record) = tracker.get_event(&event.condition_id) {
                record.start_price
            } else if let Some(ptb) = event.price_to_beat {
                ptb
            } else {
                return Ok(());
            }
        };

        let st = cl_spot.price;

        // Compute realized vol from Chainlink history (5min rolling)
        let sigma = cl_spot
            .volatility(300)
            .and_then(|v| v.to_f64())
            .unwrap_or(0.001); // fallback to 0.1% vol

        // Estimate probability using log-normal model
        let p_hat = probability::estimate_probability(s0, st, sigma, time_remaining, 0.0);

        // Determine direction and get market ask price
        let (up_ask, down_ask) = self.get_pm_prices(pm_cache, &event).await;
        let (direction, market_ask) = if p_hat > 0.5 {
            match up_ask {
                Some(ask) => (Direction::Up, ask),
                None => return Ok(()),
            }
        } else {
            match down_ask {
                Some(ask) => (Direction::Down, ask),
                None => return Ok(()),
            }
        };
        let effective_p = if direction == Direction::Up {
            p_hat
        } else {
            1.0 - p_hat
        };

        // Compute all-in cost
        let best_bid = if direction == Direction::Up {
            pm_cache
                .get(&event.up_token_id)
                .and_then(|q| q.best_bid)
                .unwrap_or(market_ask)
        } else {
            pm_cache
                .get(&event.down_token_id)
                .and_then(|q| q.best_bid)
                .unwrap_or(market_ask)
        };
        let depth_ratio = dec!(0.3); // conservative default
        let cost = self
            .fee_model
            .all_in_cost(market_ask, best_bid, market_ask, depth_ratio);
        let cost_total_f64 = cost.total.to_f64().unwrap_or(0.02);
        let market_ask_f64 = market_ask.to_f64().unwrap_or(0.5);

        // EV_net check
        let ev_net = effective_p - market_ask_f64 - cost_total_f64;

        debug!(
            "🔮 {} {} p_hat={:.3} effective_p={:.3} ask={:.3} cost={:.4} ev_net={:.4} threshold={:.3}",
            binance_symbol, direction, p_hat, effective_p,
            market_ask_f64, cost_total_f64, ev_net, self.entry_threshold
        );

        if ev_net < self.entry_threshold {
            return Ok(());
        }

        // Get Binance features for logging (used in future calibration)
        let (_momentum_10s, _momentum_60s) =
            if let Some(spot) = binance_cache.get(&binance_symbol).await {
                (
                    spot.momentum(10).and_then(|m| m.to_f64()).unwrap_or(0.0),
                    spot.momentum(60).and_then(|m| m.to_f64()).unwrap_or(0.0),
                )
            } else {
                (0.0, 0.0)
            };

        info!(
            "🔮 DIRECTIONAL ENTRY: {} {} p_hat={:.1}% ev_net={:.1}% ask={:.1}¢ cost={:.2}% σ={:.4}",
            binance_symbol,
            direction,
            effective_p * 100.0,
            ev_net * 100.0,
            market_ask_f64 * 100.0,
            cost_total_f64 * 100.0,
            sigma,
        );

        // Create signal and enter via existing maybe_enter path
        let cex_move_pct = Decimal::try_from((st - s0) / s0).unwrap_or(Decimal::ZERO);
        let edge = Decimal::try_from(ev_net).unwrap_or(Decimal::ZERO);
        let signal = MomentumSignal {
            symbol: binance_symbol,
            direction,
            cex_move_pct,
            pm_price: market_ask,
            edge,
            confidence: effective_p,
            timestamp: Utc::now(),
        };

        self.maybe_enter(signal, &event).await?;

        // If we entered, update the position with p_hat and S0
        {
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions
                .values_mut()
                .find(|p| p.condition_id == event.condition_id)
            {
                pos.entry_p_hat = Some(p_hat);
                pos.window_open_price = Some(s0);
            }
        }

        Ok(())
    }

    /// Handle Polymarket quote update - check exit conditions and dump signals
    async fn on_pm_update(&self, update: &QuoteUpdate) -> Result<()> {
        // Update dump hedge price tracker if enabled
        if let Some(ref dump_hedge) = self.dump_hedge {
            if let Some(ask) = update.quote.best_ask {
                dump_hedge
                    .on_simple_price_update(&update.token_id, ask)
                    .await;
            }
        }

        // Probability-driven exit check for directional mode positions
        if let Some(ref cl_cache) = self.chainlink_cache {
            let positions = self.positions.read().await;
            if let Some((key, pos)) = positions
                .iter()
                .find(|(_, p)| p.token_id == update.token_id)
            {
                if let (Some(entry_p), Some(s0)) = (pos.entry_p_hat, pos.window_open_price) {
                    let key = key.clone();
                    let direction = pos.direction;
                    let time_remaining = pos.time_to_resolution().num_seconds() as f64;

                    // Map Binance symbol back to Chainlink
                    if let Some(cl_symbol) =
                        crate::adapters::chainlink_rtds::to_chainlink_symbol(&pos.symbol)
                    {
                        if let Some(cl_spot) = cl_cache.get(cl_symbol).await {
                            let sigma = cl_spot
                                .volatility(300)
                                .and_then(|v| v.to_f64())
                                .unwrap_or(0.001);
                            let current_p_hat = probability::estimate_probability(
                                s0,
                                cl_spot.price,
                                sigma,
                                time_remaining,
                                0.0,
                            );
                            let effective_p = if direction == Direction::Up {
                                current_p_hat
                            } else {
                                1.0 - current_p_hat
                            };
                            let entry_effective = if direction == Direction::Up {
                                entry_p
                            } else {
                                1.0 - entry_p
                            };

                            // Probability stop: p_hat drops below 60% of entry
                            if effective_p < entry_effective * 0.6 {
                                if let Some(bid) = update.quote.best_bid {
                                    drop(positions);
                                    let reason = ExitReason::ProbabilityStop {
                                        entry_p_hat: entry_effective,
                                        current_p_hat: effective_p,
                                    };
                                    self.execute_exit(&key, bid, reason).await?;
                                    return Ok(());
                                }
                            }

                            // Time stop: < 30s remaining AND negative EV
                            if time_remaining < 30.0 {
                                let ask_f64 = update
                                    .quote
                                    .best_ask
                                    .and_then(|a| a.to_f64())
                                    .unwrap_or(0.5);
                                let cost = self
                                    .fee_model
                                    .effective_rate(update.quote.best_ask.unwrap_or(dec!(0.5)))
                                    .to_f64()
                                    .unwrap_or(0.015);
                                let ev_net = effective_p - ask_f64 - cost;
                                if ev_net < 0.0 {
                                    if let Some(bid) = update.quote.best_bid {
                                        drop(positions);
                                        self.execute_exit(&key, bid, ExitReason::TimeExit).await?;
                                        return Ok(());
                                    }
                                }
                            }

                            // Hard stop: unrealized loss > $5
                            if let Some(bid) = update.quote.best_bid {
                                let unrealized_pnl =
                                    (bid - pos.entry_price) * Decimal::from(pos.shares);
                                if unrealized_pnl < dec!(-5) {
                                    drop(positions);
                                    let reason = ExitReason::HardStop {
                                        loss_usd: -unrealized_pnl,
                                    };
                                    self.execute_exit(&key, bid, reason).await?;
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }

        // CRYINGLITTLEBABY mode: skip traditional exit checks, hold to resolution for $1
        if self.config.hold_to_resolution && self.chainlink_cache.is_none() {
            return Ok(()); // No early exits - positions resolve automatically
        }

        let mut positions = self.positions.write().await;

        // Find position matching this token
        let pos_key = positions
            .iter()
            .find(|(_, p)| p.token_id == update.token_id)
            .map(|(k, _)| k.clone());

        if let Some(key) = pos_key {
            if let Some(pos) = positions.get_mut(&key) {
                // Update highest price
                if let Some(bid) = update.quote.best_bid {
                    pos.update_high(bid);

                    // Check exit conditions
                    if let Some(reason) = self.exit_manager.check_exit(pos, bid) {
                        drop(positions); // Release lock before executing
                        self.execute_exit(&key, bid, reason).await?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Maybe enter a position based on signal
    async fn maybe_enter(&self, signal: MomentumSignal, event: &EventInfo) -> Result<()> {
        // Check daily trade limit
        if self.daily_limit_reached().await {
            debug!(
                "Daily trade limit reached ({}), skipping",
                self.config.max_daily_trades
            );
            return Ok(());
        }

        // Check cooldown first (fast check)
        if self.in_cooldown(&signal.symbol).await {
            debug!("{} in cooldown, skipping", signal.symbol);
            return Ok(());
        }

        let _entry_guard = self.entry_mutex.lock().await;

        // CRITICAL: Check if we already have a position in this symbol or event
        // This prevents duplicate orders from momentum + volatility signals
        {
            let positions = self.positions.read().await;

            // Check by symbol
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!(
                    "Already have position in {}, skipping duplicate entry",
                    signal.symbol
                );
                return Ok(());
            }

            // Check by condition_id (same event)
            if positions
                .values()
                .any(|p| p.condition_id == event.condition_id)
            {
                debug!(
                    "Already have position in event {}, skipping",
                    event.condition_id
                );
                return Ok(());
            }
        }

        // Calculate window ID for this event
        let window_id = WindowRiskTracker::window_id(&event.end_time);

        // Check window exposure limit (cross-symbol risk control)
        let estimated_cost = signal.pm_price * Decimal::from(self.config.shares_per_trade);
        {
            let tracker = self.window_tracker.read().await;

            // Check if window already has an executed trade (best_edge_only mode)
            if self.config.best_edge_only && tracker.has_executed(&window_id) {
                debug!(
                    "Window {} already has trade, skipping {}",
                    window_id, signal.symbol
                );
                return Ok(());
            }

            // Check exposure limit
            if self.config.max_window_exposure_usd > Decimal::ZERO {
                let current_exposure = tracker.get_exposure(&window_id);
                if current_exposure + estimated_cost > self.config.max_window_exposure_usd {
                    debug!(
                        "Window {} exposure ${:.2} + ${:.2} would exceed limit ${:.2}",
                        window_id,
                        current_exposure,
                        estimated_cost,
                        self.config.max_window_exposure_usd
                    );
                    return Ok(());
                }
            }
        }

        // If best_edge_only mode, queue signal for later selection
        if self.config.best_edge_only {
            let pending = PendingSignal {
                signal: signal.clone(),
                event: event.clone(),
                edge: signal.edge,
                cost_usd: estimated_cost,
                timestamp: Utc::now(),
            };

            {
                let mut tracker = self.window_tracker.write().await;
                tracker.add_pending_signal(&window_id, pending);
            }

            info!(
                "📋 Queued: {} {} edge={:.2}% (window {})",
                signal.symbol,
                signal.direction,
                signal.edge * dec!(100),
                window_id
            );

            return Ok(());
        }

        // Determine base shares to trade - use fund manager if available
        let base_shares = if let Some(ref fm) = self.fund_manager {
            // Use fund manager for balance check and position sizing
            match fm
                .can_open_position(&event.condition_id, &signal.symbol, signal.pm_price)
                .await
            {
                Ok(PositionSizeResult::Approved { shares, amount_usd }) => {
                    info!(
                        "💰 Fund manager approved: {} shares @ {:.2}¢ = ${:.2}",
                        shares,
                        signal.pm_price * dec!(100),
                        amount_usd
                    );
                    shares
                }
                Ok(PositionSizeResult::Rejected(reason)) => {
                    debug!("Fund manager rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    // Don't fall back to CLI shares - this bypasses risk management!
                    warn!("Fund manager error: {}, skipping trade for safety", e);
                    return Ok(());
                }
            }
        } else {
            // No fund manager - check max positions limit
            let positions = self.positions.read().await;
            if positions.len() >= self.config.max_positions {
                debug!(
                    "Max positions reached ({}), skipping",
                    self.config.max_positions
                );
                return Ok(());
            }
            // Position duplicate check already done above
            drop(positions);
            self.config.shares_per_trade
        };
        let shares_to_trade = self.apply_signal_position_sizing(base_shares, &signal);
        if shares_to_trade < 5 {
            debug!(
                "Position size {} below Polymarket minimum 5 shares (base={})",
                shares_to_trade, base_shares
            );
            return Ok(());
        }

        // Execute entry
        let token_id = match signal.direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        // Log entry signal with mode-specific info
        let time_remaining = event.time_remaining().num_seconds();
        if self.config.hold_to_resolution {
            info!(
                "🎯 CONFIRMATORY ENTRY: {} {} @ {:.2}¢ | {}s to resolution | CEX: {:.2}%",
                signal.symbol,
                signal.direction,
                signal.pm_price * dec!(100),
                time_remaining,
                signal.cex_move_pct * dec!(100),
            );
            info!(
                "   → Expected payout: $1.00 (profit: {:.0}¢ per share)",
                (dec!(1) - signal.pm_price) * dec!(100)
            );
        } else {
            info!(
                "ENTRY SIGNAL: {} {} @ {:.2}¢ (CEX move: {:.2}%, edge: {:.2}%, conf: {:.0}%)",
                signal.symbol,
                signal.direction,
                signal.pm_price * dec!(100),
                signal.cex_move_pct * dec!(100),
                signal.edge * dec!(100),
                signal.confidence * 100.0,
            );
        }

        if self.dry_run {
            let expected_profit = if self.config.hold_to_resolution {
                let profit_per_share = dec!(1) - signal.pm_price;
                format!(
                    " → Expected: ${:.2}",
                    profit_per_share * Decimal::from(shares_to_trade)
                )
            } else {
                String::new()
            };
            info!(
                "[DRY RUN] Would buy {} shares of {} {}{}",
                shares_to_trade, signal.symbol, signal.direction, expected_profit
            );
        } else {
            // Create and execute order with calculated shares
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                shares_to_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let tracked_shares = if result.filled_shares > 0 {
                        result.filled_shares
                    } else {
                        shares_to_trade
                    };
                    let entry_notional = fill_price * Decimal::from(tracked_shares);
                    let trade_count = self.record_trade().await;
                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        tracked_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record position with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened_with_amount(
                            &event.condition_id,
                            &signal.symbol,
                            entry_notional,
                        )
                        .await;
                    }

                    // Track position in local state
                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        entry_notional,
                        shares: tracked_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                        condition_id: event.condition_id.clone(),
                        entry_p_hat: None,
                        window_open_price: None,
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);

                    // Log trade entry
                    if let Some(ref logger) = self.trade_logger {
                        logger
                            .record_entry(
                                &signal.symbol,
                                &event.slug,
                                &event.condition_id,
                                &format!("{}", signal.direction),
                                fill_price,
                                tracked_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
                    // Update cooldown only on confirmed fill
                    let mut last_trade = self.last_trade_time.write().await;
                    last_trade.insert(signal.symbol.clone(), Utc::now());
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        Ok(())
    }

    /// Execute position exit
    async fn execute_exit(&self, symbol: &str, price: Decimal, reason: ExitReason) -> Result<()> {
        let position = {
            let mut positions = self.positions.write().await;
            match positions.remove(symbol) {
                Some(p) => p,
                None => return Ok(()),
            }
        };

        let pnl_pct = position.pnl_pct(price);
        let pnl_usd = pnl_pct * Decimal::from(position.shares) * position.entry_price;

        info!(
            "EXIT: {} {} @ {:.2}¢ - {} (P&L: {:.2}% / ${:.2})",
            symbol,
            position.direction,
            price * dec!(100),
            reason,
            pnl_pct * dec!(100),
            pnl_usd,
        );

        let mut closed = self.dry_run;

        if self.dry_run {
            info!("[DRY RUN] Would sell {} shares", position.shares);
        } else {
            // Create sell order
            let order = OrderRequest::sell_limit(
                position.token_id.clone(),
                position.direction.into(),
                position.shares,
                price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let exit_price = result.avg_fill_price.unwrap_or(price);
                    info!(
                        "Exit filled: {} shares @ {:.2}¢",
                        result.filled_shares,
                        exit_price * dec!(100)
                    );
                    closed = true;
                }
                Err(e) => {
                    error!("Exit order failed: {}", e);
                    // Re-add position on failure
                    let mut positions = self.positions.write().await;
                    positions.insert(symbol.to_string(), position.clone());
                    closed = false;
                }
            }
        }

        if closed {
            if let Some(ref fm) = self.fund_manager {
                let released_notional = if position.entry_notional > Decimal::ZERO {
                    position.entry_notional
                } else {
                    position.entry_price * Decimal::from(position.shares)
                };
                fm.record_position_closed_with_amount(
                    &position.condition_id,
                    &position.symbol,
                    released_notional,
                )
                .await;
            }
        }

        Ok(())
    }

    /// Check if symbol is in cooldown period
    async fn in_cooldown(&self, symbol: &str) -> bool {
        let last_trade = self.last_trade_time.read().await;

        if let Some(last_time) = last_trade.get(symbol) {
            let elapsed = Utc::now() - *last_time;
            return elapsed.num_seconds() < self.config.cooldown_secs as i64;
        }

        false
    }

    /// Process pending signals and execute best edge (if ready)
    async fn process_pending_signals(&self) -> Result<()> {
        if !self.config.best_edge_only {
            return Ok(());
        }

        let ready_windows = {
            let tracker = self.window_tracker.read().await;
            tracker.get_ready_windows(self.config.signal_collection_delay_ms)
        };

        for window_id in ready_windows {
            // Get the best signal for this window
            let best_signal = {
                let tracker = self.window_tracker.read().await;

                // Skip if already executed
                if tracker.has_executed(&window_id) {
                    continue;
                }

                tracker.get_best_signal(&window_id)
            };

            if let Some(pending) = best_signal {
                // Check window exposure limit
                let can_execute = {
                    let tracker = self.window_tracker.read().await;
                    let current_exposure = tracker.get_exposure(&window_id);
                    let max_exposure = self.config.max_window_exposure_usd;

                    max_exposure == Decimal::ZERO
                        || current_exposure + pending.cost_usd <= max_exposure
                };

                if can_execute {
                    info!(
                        "🏆 Best edge selected: {} {} edge={:.2}% (window {})",
                        pending.signal.symbol,
                        pending.signal.direction,
                        pending.edge * dec!(100),
                        window_id
                    );

                    // Execute the trade directly
                    self.execute_pending_trade(pending.clone()).await?;

                    // Mark window as executed and add exposure
                    {
                        let mut tracker = self.window_tracker.write().await;
                        tracker.mark_executed(&window_id);
                        tracker.add_exposure(&window_id, pending.cost_usd);
                        tracker.clear_pending(&window_id);
                    }
                } else {
                    info!(
                        "⚠️ Window {} at exposure limit, skipping {}",
                        window_id, pending.signal.symbol
                    );

                    // Clear pending signals for this window
                    let mut tracker = self.window_tracker.write().await;
                    tracker.clear_pending(&window_id);
                }
            }
        }

        // Periodic cleanup
        {
            let mut tracker = self.window_tracker.write().await;
            tracker.cleanup_old();
        }

        Ok(())
    }

    /// Execute a pending trade
    async fn execute_pending_trade(&self, pending: PendingSignal) -> Result<()> {
        let signal = &pending.signal;
        let event = &pending.event;
        let _entry_guard = self.entry_mutex.lock().await;

        // Re-check if we already have position (might have changed since queueing)
        {
            let positions = self.positions.read().await;
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!("Already have position in {}, skipping", signal.symbol);
                return Ok(());
            }
            if positions
                .values()
                .any(|p| p.condition_id == event.condition_id)
            {
                debug!(
                    "Already have position in event {}, skipping",
                    event.condition_id
                );
                return Ok(());
            }
        }

        // Get base position size
        let base_shares = if let Some(ref fm) = self.fund_manager {
            match fm
                .can_open_position(&event.condition_id, &signal.symbol, signal.pm_price)
                .await
            {
                Ok(PositionSizeResult::Approved { shares, amount_usd }) => {
                    info!(
                        "💰 Fund manager approved: {} shares @ {:.2}¢ = ${:.2}",
                        shares,
                        signal.pm_price * dec!(100),
                        amount_usd
                    );
                    shares
                }
                Ok(PositionSizeResult::Rejected(reason)) => {
                    debug!("Fund manager rejected: {}", reason);
                    return Ok(());
                }
                Err(e) => {
                    // Don't fall back to CLI shares - this bypasses risk management!
                    warn!("Fund manager error: {}, skipping trade for safety", e);
                    return Ok(());
                }
            }
        } else {
            self.config.shares_per_trade
        };
        let shares_to_trade = self.apply_signal_position_sizing(base_shares, signal);
        if shares_to_trade < 5 {
            debug!(
                "Position size {} below Polymarket minimum 5 shares (base={})",
                shares_to_trade, base_shares
            );
            return Ok(());
        }

        // Execute entry
        let token_id = match signal.direction {
            Direction::Up => &event.up_token_id,
            Direction::Down => &event.down_token_id,
        };

        if self.dry_run {
            info!(
                "[DRY RUN] Best edge trade: {} {} {} shares @ {:.2}¢",
                signal.symbol,
                signal.direction,
                shares_to_trade,
                signal.pm_price * dec!(100)
            );
        } else {
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                shares_to_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let tracked_shares = if result.filled_shares > 0 {
                        result.filled_shares
                    } else {
                        shares_to_trade
                    };
                    let entry_notional = fill_price * Decimal::from(tracked_shares);
                    let trade_count = self.record_trade().await;

                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        tracked_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened_with_amount(
                            &event.condition_id,
                            &signal.symbol,
                            entry_notional,
                        )
                        .await;
                    }

                    // Track position
                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        entry_notional,
                        shares: tracked_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                        condition_id: event.condition_id.clone(),
                        entry_p_hat: None,
                        window_open_price: None,
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);

                    // Log trade
                    if let Some(ref logger) = self.trade_logger {
                        logger
                            .record_entry(
                                &signal.symbol,
                                &event.slug,
                                &event.condition_id,
                                &format!("{}", signal.direction),
                                fill_price,
                                tracked_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
                    // Update cooldown only on confirmed fill
                    let mut last_trade = self.last_trade_time.write().await;
                    last_trade.insert(signal.symbol.clone(), Utc::now());
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_direction_opposite() {
        assert_eq!(Direction::Up.opposite(), Direction::Down);
        assert_eq!(Direction::Down.opposite(), Direction::Up);
    }

    #[test]
    fn test_momentum_signal_valid() {
        let config = MomentumConfig::default();
        let signal = MomentumSignal {
            symbol: "BTCUSDT".into(),
            direction: Direction::Up,
            cex_move_pct: dec!(0.01), // 1% (>= min_move_pct of 0.3%)
            pm_price: dec!(0.30),     // 30¢ (<= max_entry_price of 35¢)
            edge: dec!(0.10),         // 10% (>= min_edge of 3%)
            confidence: 0.8,
            timestamp: Utc::now(),
        };

        assert!(signal.is_valid(&config));
    }

    #[test]
    fn test_position_pnl() {
        let pos = Position {
            token_id: "test".into(),
            symbol: "BTCUSDT".into(),
            direction: Direction::Up,
            entry_price: dec!(0.50),
            entry_notional: dec!(50),
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
            condition_id: "test_condition".into(),
            entry_p_hat: None,
            window_open_price: None,
        };

        // 10% profit
        assert_eq!(pos.pnl_pct(dec!(0.55)), dec!(0.10));

        // 10% loss
        assert_eq!(pos.pnl_pct(dec!(0.45)), dec!(-0.10));
    }

    #[test]
    fn test_exit_manager_take_profit() {
        let config = ExitConfig {
            take_profit_pct: dec!(0.20),
            stop_loss_pct: dec!(0.15),
            trailing_stop_pct: dec!(0.10),
            exit_before_resolution_secs: 30,
        };

        let manager = ExitManager::new(config);

        let pos = Position {
            token_id: "test".into(),
            symbol: "BTCUSDT".into(),
            direction: Direction::Up,
            entry_price: dec!(0.50),
            entry_notional: dec!(50),
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
            condition_id: "test_condition".into(),
            entry_p_hat: None,
            window_open_price: None,
        };

        // 25% profit should trigger take profit
        let exit = manager.check_exit(&pos, dec!(0.625));
        assert!(matches!(exit, Some(ExitReason::TakeProfit { .. })));
    }

    #[test]
    fn test_exit_manager_stop_loss() {
        let config = ExitConfig::default();
        let manager = ExitManager::new(config);

        let pos = Position {
            token_id: "test".into(),
            symbol: "BTCUSDT".into(),
            direction: Direction::Up,
            entry_price: dec!(0.50),
            entry_notional: dec!(50),
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
            condition_id: "test_condition".into(),
            entry_p_hat: None,
            window_open_price: None,
        };

        // 20% loss should trigger stop loss
        let exit = manager.check_exit(&pos, dec!(0.40));
        assert!(matches!(exit, Some(ExitReason::StopLoss { .. })));
    }

    #[test]
    fn test_parse_price_from_question() {
        // Test various Polymarket question formats

        // Standard format with dollar sign and commas
        assert_eq!(
            EventInfo::parse_price_from_question("Will BTC be above $94,000 at 9:15 PM?"),
            Some(dec!(94000))
        );

        // With decimals
        assert_eq!(
            EventInfo::parse_price_from_question("Will ETH be above $3,500.50 at 10:00 AM?"),
            Some(dec!(3500.50))
        );

        // Without dollar sign (outcome format like "↑ 94,000")
        assert_eq!(
            EventInfo::parse_price_from_question("↑ 94,000"),
            Some(dec!(94000))
        );

        // Down arrow format
        assert_eq!(
            EventInfo::parse_price_from_question("↓ 86,000"),
            Some(dec!(86000))
        );

        // Large numbers
        assert_eq!(
            EventInfo::parse_price_from_question("Will BTC be above $100,000?"),
            Some(dec!(100000))
        );

        // Small numbers (SOL)
        assert_eq!(
            EventInfo::parse_price_from_question("Will SOL be above $150.25?"),
            Some(dec!(150.25))
        );

        // No price in question
        assert_eq!(
            EventInfo::parse_price_from_question("Will it rain tomorrow?"),
            None
        );

        // Empty string
        assert_eq!(EventInfo::parse_price_from_question(""), None);
    }

    #[test]
    fn test_event_matcher_includes_btc_5m_series() {
        let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        let matcher = EventMatcher::new(client);

        let btc_series = matcher
            .symbol_to_series
            .get("BTCUSDT")
            .expect("BTCUSDT mapping should exist");

        assert!(
            btc_series.iter().any(|id| id == "10684"),
            "BTCUSDT series should include 5m series id 10684"
        );
    }

    #[tokio::test]
    async fn test_find_event_with_timing_prefers_best_across_all_series() {
        let client = PolymarketClient::new("https://clob.polymarket.com", true).unwrap();
        let mut matcher = EventMatcher::new(client);

        matcher
            .symbol_to_series
            .insert("BTCUSDT".into(), vec!["series-a".into(), "series-b".into()]);

        let now = Utc::now();
        let mk_event = |slug: &str, seconds_remaining: i64| EventInfo {
            slug: slug.to_string(),
            title: slug.to_string(),
            up_token_id: format!("{slug}-up"),
            down_token_id: format!("{slug}-down"),
            start_time: now,
            end_time: now + ChronoDuration::seconds(seconds_remaining),
            condition_id: format!("{slug}-condition"),
            series_id: "test".to_string(),
            horizon: "other".to_string(),
            price_to_beat: None,
        };

        {
            let mut events = matcher.active_events.write().await;
            events.insert("series-a".into(), vec![mk_event("event-a", 600)]);
            events.insert("series-b".into(), vec![mk_event("event-b", 120)]);
        }

        let best = matcher
            .find_event_with_timing("BTCUSDT", 60, 900, true)
            .await
            .expect("expected event");

        assert_eq!(best.slug, "event-b");
    }
}
