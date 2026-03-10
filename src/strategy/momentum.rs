//! Momentum strategy for Polymarket trading
//!
//! Implements the "gabagool22" style strategy:
//! 1. Monitor CEX (Binance) for BTC/ETH/SOL price movements
//! 2. When spot price moves significantly, Polymarket odds lag
//! 3. Enter the side that should win before odds adjust
//! 4. Exit via take-profit, stop-loss, trailing stop, or hold to resolution

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{debug, error, info, trace, warn};

use crate::adapters::{
    ChainlinkPriceCache, ChainlinkUpdate, GammaEventInfo, PolymarketClient, PriceCache,
    PriceUpdate, QuoteCache, QuoteUpdate, SpotPrice,
};
use crate::config::RiskConfig;
use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::platform::CryptoDataPlaneHandle;
use crate::strategy::OrderExecutor;
use crate::strategy::crypto::{
    horizon_for_series as crypto_horizon_for_series, known_binance_symbols,
    series_ids_for_symbol as crypto_series_ids_for_symbol,
};
use crate::strategy::dump_hedge::{DumpHedgeConfig, DumpHedgeEngine};
use crate::strategy::fee_model::FeeModel;
use crate::strategy::fund_manager::{FundManager, PositionSizeResult};
use crate::strategy::probability;
use crate::strategy::volatility::{EventTracker, VolatilityConfig, VolatilityDetector};

mod detector;
mod entry_runtime;
mod matcher;
mod pm_runtime;
mod position_exit;
mod runtime_state;
mod trade_flow;

pub use detector::{MomentumDetector, MomentumSignal};
pub use matcher::{EventInfo, EventMatcher};
pub use position_exit::{ExitManager, ExitReason, Position};
use self::runtime_state::{DailyTradeCounter, PendingSignal, WindowRiskTracker};

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
// Momentum Engine
// ============================================================================

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

    /// Get event matcher reference
    pub fn event_matcher(&self) -> &EventMatcher {
        &self.event_matcher
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
