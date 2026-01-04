//! Momentum strategy for Polymarket trading
//!
//! Implements the "gabagool22" style strategy:
//! 1. Monitor CEX (Binance) for BTC/ETH/SOL price movements
//! 2. When spot price moves significantly, Polymarket odds lag
//! 3. Enter the side that should win before odds adjust
//! 4. Exit via take-profit, stop-loss, trailing stop, or hold to resolution

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use crate::adapters::{
    BinanceWebSocket, GammaEventInfo, PolymarketClient, PriceCache, PriceUpdate, QuoteCache,
    QuoteUpdate, SpotPrice,
};
use crate::domain::{OrderRequest, Side};
use crate::error::{PloyError, Result};
use crate::strategy::OrderExecutor;

// ============================================================================
// Configuration
// ============================================================================

/// Momentum strategy configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MomentumConfig {
    /// Minimum CEX price move to trigger (e.g., 0.003 = 0.3%)
    pub min_move_pct: Decimal,

    /// Maximum Polymarket odds for entry (e.g., 0.40 = 40¢)
    /// Lower = better entry like CRYINGLITTLEBABY style (20-30¢)
    pub max_entry_price: Decimal,

    /// Minimum estimated edge to enter (e.g., 0.03 = 3%)
    pub min_edge: Decimal,

    /// Lookback window for momentum calculation (seconds)
    pub lookback_secs: u64,

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
}

impl Default for MomentumConfig {
    fn default() -> Self {
        Self {
            // === AGGRESSIVE ENTRY (CRYINGLITTLEBABY style) ===
            min_move_pct: dec!(0.003),      // 0.3% minimum move
            max_entry_price: dec!(0.35),    // Max 35¢ entry (confirmed winner should be cheap)
            min_edge: dec!(0.03),           // 3% minimum edge
            lookback_secs: 5,               // 5-second momentum window
            shares_per_trade: 100,          // ~$35 per trade at 35¢

            // === ANTI-OVERTRADING CONTROLS ===
            max_positions: 3,               // Max 3 concurrent
            cooldown_secs: 60,              // 60s between same symbol
            max_daily_trades: 20,           // Max 20 trades/day

            symbols: vec![
                "BTCUSDT".into(),
                "ETHUSDT".into(),
                "SOLUSDT".into(),
                "XRPUSDT".into(),
            ],

            // === CRYINGLITTLEBABY CONFIRMATORY MODE (DEFAULT: ON) ===
            hold_to_resolution: true,       // Hold to collect $1
            min_time_remaining_secs: 60,    // Min 1 min left
            max_time_remaining_secs: 300,   // Max 5 min left (outcome should be decided)
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
            take_profit_pct: dec!(0.20),      // +20% take profit
            stop_loss_pct: dec!(0.15),        // -15% stop loss
            trailing_stop_pct: dec!(0.10),    // 10% trailing from high
            exit_before_resolution_secs: 30,  // Exit 30s before end
        }
    }
}

// ============================================================================
// Event Matcher
// ============================================================================

/// Maps CEX symbols to Polymarket event series
pub struct EventMatcher {
    client: PolymarketClient,
    /// Map symbol to series ID
    symbol_to_series: HashMap<String, Vec<String>>,
    /// Cache of active events per series
    active_events: Arc<RwLock<HashMap<String, Vec<EventInfo>>>>,
}

/// Event information for trading
#[derive(Debug, Clone)]
pub struct EventInfo {
    pub slug: String,
    pub title: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub end_time: DateTime<Utc>,
    pub condition_id: String,
}

impl EventInfo {
    /// Time remaining until event resolution
    pub fn time_remaining(&self) -> ChronoDuration {
        self.end_time - Utc::now()
    }

    /// Check if event is still tradeable (has enough time remaining)
    pub fn is_tradeable(&self, min_seconds: i64) -> bool {
        self.time_remaining().num_seconds() > min_seconds
    }
}

impl EventMatcher {
    /// Create a new event matcher
    pub fn new(client: PolymarketClient) -> Self {
        let mut symbol_to_series = HashMap::new();

        // Map each symbol to its series IDs (from Gamma API)
        // === 15-MINUTE MARKETS ONLY (CRYINGLITTLEBABY style) ===

        // BTC: 10192 = 15m (btc-up-or-down-15m)
        symbol_to_series.insert(
            "BTCUSDT".into(),
            vec![
                "10192".into(), // btc-up-or-down-15m
            ],
        );

        // ETH: 10191 = 15m ONLY
        symbol_to_series.insert(
            "ETHUSDT".into(),
            vec![
                "10191".into(), // eth-up-or-down-15m
            ],
        );

        // SOL: 10423 = 15m ONLY
        symbol_to_series.insert(
            "SOLUSDT".into(),
            vec![
                "10423".into(), // sol-up-or-down-15m
            ],
        );

        // XRP: 10422 = 15m (xrp-up-or-down-15m)
        symbol_to_series.insert(
            "XRPUSDT".into(),
            vec![
                "10422".into(), // xrp-up-or-down-15m
            ],
        );

        Self {
            client,
            symbol_to_series,
            active_events: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Find the best event for a symbol
    /// In confirmatory mode (CRYINGLITTLEBABY): prefers events CLOSE to ending
    /// In predictive mode: prefers events with more time remaining
    pub async fn find_event(&self, symbol: &str) -> Option<EventInfo> {
        // Default: predictive mode (more time = better)
        self.find_event_with_timing(symbol, 60, i64::MAX, false).await
    }

    /// Find event with timing constraints (CRYINGLITTLEBABY confirmatory mode)
    ///
    /// # Arguments
    /// * `min_secs` - Minimum time remaining (default: 60s)
    /// * `max_secs` - Maximum time remaining (default: 300s for confirmatory mode)
    /// * `prefer_close_to_end` - If true, prefer events closest to ending (confirmatory mode)
    pub async fn find_event_with_timing(
        &self,
        symbol: &str,
        min_secs: u64,
        max_secs: i64,
        prefer_close_to_end: bool,
    ) -> Option<EventInfo> {
        let series_ids = self.symbol_to_series.get(symbol)?;
        let events = self.active_events.read().await;

        // Search through all series for this symbol
        for series_id in series_ids {
            if let Some(series_events) = events.get(series_id) {
                // Filter to events within time window
                let filtered: Vec<_> = series_events
                    .iter()
                    .filter(|e| {
                        let remaining = e.time_remaining().num_seconds();
                        remaining >= min_secs as i64 && remaining <= max_secs
                    })
                    .collect();

                if filtered.is_empty() {
                    continue;
                }

                // CRYINGLITTLEBABY: prefer events CLOSEST to ending (less time = better)
                // Predictive: prefer events with MORE time remaining
                let best = if prefer_close_to_end {
                    filtered.into_iter().min_by_key(|e| e.time_remaining().num_seconds())
                } else {
                    filtered.into_iter().max_by_key(|e| e.time_remaining().num_seconds())
                };

                if let Some(event) = best {
                    return Some(event.clone());
                }
            }
        }

        None
    }

    /// Get all tradeable events for a symbol
    pub async fn get_events(&self, symbol: &str) -> Vec<EventInfo> {
        let series_ids = match self.symbol_to_series.get(symbol) {
            Some(ids) => ids,
            None => return vec![],
        };

        let events = self.active_events.read().await;
        let mut result = vec![];

        for series_id in series_ids {
            if let Some(series_events) = events.get(series_id) {
                for event in series_events {
                    if event.is_tradeable(60) {
                        result.push(event.clone());
                    }
                }
            }
        }

        result
    }

    /// Refresh active events from Polymarket API
    pub async fn refresh(&self) -> Result<()> {
        let mut events = self.active_events.write().await;

        for (_, series_ids) in &self.symbol_to_series {
            for series_id in series_ids {
                match self.fetch_series_events(series_id).await {
                    Ok(series_events) => {
                        events.insert(series_id.clone(), series_events);
                    }
                    Err(e) => {
                        warn!("Failed to fetch events for {}: {}", series_id, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// Fetch events for a specific series
    async fn fetch_series_events(&self, series_id: &str) -> Result<Vec<EventInfo>> {
        let gamma_events = self.client.get_all_active_events(series_id).await?;

        let mut events = vec![];
        // Limit to first 5 events to avoid too many API calls
        for gamma_event in gamma_events.into_iter().take(5) {
            // Get full event details to access market condition_id
            let event_details = match self.client.get_event_details(&gamma_event.id).await {
                Ok(details) => details,
                Err(e) => {
                    debug!("Failed to get details for event {}: {}", gamma_event.id, e);
                    continue;
                }
            };

            // Get condition_id from event details
            let market = match event_details.markets.first() {
                Some(m) => m,
                None => continue,
            };
            let condition_id = match &market.condition_id {
                Some(cid) => cid.clone(),
                None => continue,
            };

            // Fetch market from CLOB API to get actual token IDs
            let clob_market = match self.client.get_market(&condition_id).await {
                Ok(m) => m,
                Err(e) => {
                    debug!("Failed to get CLOB market for {}: {}", condition_id, e);
                    continue;
                }
            };

            // Find UP and DOWN tokens from CLOB market
            let up_token = clob_market.tokens.iter().find(|t| {
                let outcome = t.outcome.to_lowercase();
                outcome.contains("up") || outcome == "yes"
            });
            let down_token = clob_market.tokens.iter().find(|t| {
                let outcome = t.outcome.to_lowercase();
                outcome.contains("down") || outcome == "no"
            });

            if let (Some(up), Some(down)) = (up_token, down_token) {
                // Parse end time
                let end_time = match event_details.end_date.as_ref().and_then(|s| {
                    DateTime::parse_from_rfc3339(s)
                        .map(|dt| dt.with_timezone(&Utc))
                        .ok()
                }) {
                    Some(t) => t,
                    None => continue,
                };

                let event_info = EventInfo {
                    slug: event_details.slug.clone().unwrap_or_default(),
                    title: event_details.title.clone().unwrap_or_default(),
                    up_token_id: up.token_id.clone(),
                    down_token_id: down.token_id.clone(),
                    end_time,
                    condition_id,
                };

                debug!(
                    "Found event: {} (UP={}, DOWN={})",
                    event_info.title,
                    &event_info.up_token_id[..20.min(event_info.up_token_id.len())],
                    &event_info.down_token_id[..20.min(event_info.down_token_id.len())]
                );

                events.push(event_info);
            }
        }

        Ok(events)
    }

    /// Convert Gamma API event to EventInfo
    fn convert_gamma_event(&self, gamma: &GammaEventInfo) -> Option<EventInfo> {
        debug!("Converting event: id={}, markets={}", gamma.id, gamma.markets.len());

        let market = gamma.markets.first()?;
        let tokens = match market.tokens.as_ref() {
            Some(t) => t,
            None => {
                debug!("Event {} has no tokens", gamma.id);
                return None;
            }
        };

        debug!("Event {} has {} tokens", gamma.id, tokens.len());
        if tokens.len() < 2 {
            return None;
        }

        // Log token outcomes for debugging
        for t in tokens {
            debug!("  Token: {} = {}", t.token_id, t.outcome);
        }

        // Find UP and DOWN tokens
        let up_token = tokens.iter().find(|t| {
            let outcome = t.outcome.to_lowercase();
            outcome.contains("up") || outcome == "yes" || outcome.starts_with("↑")
        })?;
        let down_token = tokens.iter().find(|t| {
            let outcome = t.outcome.to_lowercase();
            outcome.contains("down") || outcome == "no" || outcome.starts_with("↓")
        })?;

        let end_time = gamma.end_date.as_ref().and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .ok()
        })?;

        Some(EventInfo {
            slug: gamma.slug.clone().unwrap_or_default(),
            title: gamma.title.clone().unwrap_or_default(),
            up_token_id: up_token.token_id.clone(),
            down_token_id: down_token.token_id.clone(),
            end_time,
            condition_id: market.condition_id.clone().unwrap_or_default(),
        })
    }

    /// Get all unique token IDs for WebSocket subscription
    pub async fn get_all_token_ids(&self) -> Vec<String> {
        let events = self.active_events.read().await;
        let mut token_ids = vec![];

        for series_events in events.values() {
            for event in series_events {
                token_ids.push(event.up_token_id.clone());
                token_ids.push(event.down_token_id.clone());
            }
        }

        token_ids
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
// Momentum Signal
// ============================================================================

/// A detected momentum opportunity
#[derive(Debug, Clone)]
pub struct MomentumSignal {
    pub symbol: String,
    pub direction: Direction,
    pub cex_move_pct: Decimal,
    pub pm_price: Decimal,
    pub edge: Decimal,
    pub confidence: f64,
    pub timestamp: DateTime<Utc>,
}

impl MomentumSignal {
    /// Check if signal is valid for trading
    pub fn is_valid(&self, config: &MomentumConfig) -> bool {
        self.cex_move_pct.abs() >= config.min_move_pct
            && self.pm_price <= config.max_entry_price
            && self.edge >= config.min_edge
    }
}

// ============================================================================
// Momentum Detector
// ============================================================================

/// Detects momentum opportunities by comparing CEX prices to Polymarket odds
pub struct MomentumDetector {
    config: MomentumConfig,
}

impl MomentumDetector {
    pub fn new(config: MomentumConfig) -> Self {
        Self { config }
    }

    /// Check for momentum signal given CEX and PM prices
    pub fn check(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Option<MomentumSignal> {
        // Calculate CEX momentum
        let momentum = spot.momentum(self.config.lookback_secs)?;

        // Check minimum move threshold
        if momentum.abs() < self.config.min_move_pct {
            return None;
        }

        // Determine direction and PM price
        let (direction, pm_price) = if momentum > Decimal::ZERO {
            (Direction::Up, up_ask?)
        } else {
            (Direction::Down, down_ask?)
        };

        // Check if PM price is still attractive (lagging)
        if pm_price > self.config.max_entry_price {
            debug!(
                "{} {} PM price {:.2}¢ > max {:.2}¢, skipping",
                symbol,
                direction,
                pm_price * dec!(100),
                self.config.max_entry_price * dec!(100)
            );
            return None;
        }

        // Estimate fair value and edge
        let fair_value = self.estimate_fair_value(momentum);
        let edge = fair_value - pm_price;

        if edge < self.config.min_edge {
            debug!(
                "{} {} edge {:.2}% < min {:.2}%, skipping",
                symbol,
                direction,
                edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return None;
        }

        // Calculate confidence based on momentum strength and edge
        let confidence = self.calculate_confidence(momentum, edge);

        Some(MomentumSignal {
            symbol: symbol.to_string(),
            direction,
            cex_move_pct: momentum,
            pm_price,
            edge,
            confidence,
            timestamp: Utc::now(),
        })
    }

    /// Estimate fair value based on CEX momentum
    fn estimate_fair_value(&self, momentum: Decimal) -> Decimal {
        // Simple model: larger moves correlate with higher probability
        // This should ideally be calibrated with historical data
        let base_prob = dec!(0.50);

        // Scale: 1% move adds ~10% to probability
        let momentum_factor = momentum.abs() * dec!(10);

        // Cap at 90% probability
        (base_prob + momentum_factor).min(dec!(0.90))
    }

    /// Calculate confidence score (0.0 to 1.0)
    fn calculate_confidence(&self, momentum: Decimal, edge: Decimal) -> f64 {
        // Higher momentum and edge = higher confidence
        let momentum_score = (momentum.abs() / dec!(0.02)).min(Decimal::ONE);
        let edge_score = (edge / dec!(0.15)).min(Decimal::ONE);

        // Weighted average
        let score = momentum_score * dec!(0.4) + edge_score * dec!(0.6);

        // Convert to f64, clamp to [0, 1]
        score.to_string().parse::<f64>().unwrap_or(0.5).clamp(0.0, 1.0)
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
    pub shares: u64,
    pub entry_time: DateTime<Utc>,
    pub highest_price: Decimal,
    pub event_end_time: DateTime<Utc>,
    pub event_slug: String,
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
    TakeProfit { profit_pct: Decimal },
    StopLoss { loss_pct: Decimal },
    TrailingStop { high: Decimal, current: Decimal },
    TimeExit,
    Manual,
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
                write!(f, "TrailingStop(high={:.2}¢, cur={:.2}¢)", high * dec!(100), current * dec!(100))
            }
            ExitReason::TimeExit => write!(f, "TimeExit"),
            ExitReason::Manual => write!(f, "Manual"),
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
            return Some(ExitReason::TakeProfit { profit_pct: pnl_pct });
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

/// Main engine orchestrating the momentum strategy
pub struct MomentumEngine {
    config: MomentumConfig,
    exit_config: ExitConfig,
    detector: MomentumDetector,
    exit_manager: ExitManager,
    event_matcher: EventMatcher,
    executor: OrderExecutor,
    positions: Arc<RwLock<HashMap<String, Position>>>,
    last_trade_time: Arc<RwLock<HashMap<String, DateTime<Utc>>>>,
    daily_trades: Arc<RwLock<DailyTradeCounter>>,
    dry_run: bool,
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
        }
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

    /// Get event matcher reference
    pub fn event_matcher(&self) -> &EventMatcher {
        &self.event_matcher
    }

    /// Get positions count
    pub async fn positions_count(&self) -> usize {
        self.positions.read().await.len()
    }

    /// Run the momentum strategy
    pub async fn run(
        &self,
        mut binance_rx: broadcast::Receiver<PriceUpdate>,
        mut pm_rx: broadcast::Receiver<QuoteUpdate>,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        info!("Starting momentum engine (dry_run={})", self.dry_run);

        // Log mode-specific configuration
        if self.config.hold_to_resolution {
            info!("=== CRYINGLITTLEBABY CONFIRMATORY MODE ===");
            info!("• Entry window: {}-{}s before resolution",
                self.config.min_time_remaining_secs,
                self.config.max_time_remaining_secs);
            info!("• Hold to resolution: YES (collect $1)");
            info!("• Min CEX move: {:.2}%, Max entry: {:.0}¢",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100));
        } else {
            info!("=== PREDICTIVE MODE (early entry) ===");
            info!("Config: min_move={:.2}%, max_entry={:.0}¢, min_edge={:.1}%",
                self.config.min_move_pct * dec!(100),
                self.config.max_entry_price * dec!(100),
                self.config.min_edge * dec!(100));
        }

        // Refresh events initially
        if let Err(e) = self.event_matcher.refresh().await {
            error!("Failed to refresh events: {}", e);
        }

        // Periodic event refresh
        let event_matcher = &self.event_matcher;
        let refresh_interval = tokio::time::interval(Duration::from_secs(60));
        tokio::pin!(refresh_interval);

        loop {
            tokio::select! {
                // CEX price update - check for entry signals
                Ok(price_update) = binance_rx.recv() => {
                    if let Err(e) = self.on_cex_update(&price_update, binance_cache, pm_cache).await {
                        error!("Error processing CEX update: {}", e);
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
            match self.event_matcher.find_event_with_timing(
                symbol,
                self.config.min_time_remaining_secs,
                self.config.max_time_remaining_secs as i64,
                true, // prefer_close_to_end
            ).await {
                Some(e) => e,
                None => {
                    debug!("{} no event in confirmatory window ({}-{}s)",
                        symbol,
                        self.config.min_time_remaining_secs,
                        self.config.max_time_remaining_secs);
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
            debug!("{} found event {} with {}s remaining (confirmatory mode)",
                symbol, event.title, remaining);
        }

        // Get PM quotes for this event
        let (up_ask, down_ask) = self.get_pm_prices(pm_cache, &event).await;

        // Check for momentum signal
        if let Some(signal) = self.detector.check(symbol, &spot, up_ask, down_ask) {
            self.maybe_enter(signal, &event).await?;
        }

        Ok(())
    }

    /// Get Polymarket prices for an event
    async fn get_pm_prices(
        &self,
        pm_cache: &QuoteCache,
        event: &EventInfo,
    ) -> (Option<Decimal>, Option<Decimal>) {
        let up_quote = pm_cache.get(&event.up_token_id).await;
        let down_quote = pm_cache.get(&event.down_token_id).await;

        let up_ask = up_quote.and_then(|q| q.best_ask);
        let down_ask = down_quote.and_then(|q| q.best_ask);

        (up_ask, down_ask)
    }

    /// Handle Polymarket quote update - check exit conditions
    async fn on_pm_update(&self, update: &QuoteUpdate) -> Result<()> {
        // CRYINGLITTLEBABY mode: skip exit checks, hold to resolution for $1
        if self.config.hold_to_resolution {
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
            debug!("Daily trade limit reached ({}), skipping", self.config.max_daily_trades);
            return Ok(());
        }

        // Check position limit
        let positions = self.positions.read().await;
        if positions.len() >= self.config.max_positions {
            debug!("Max positions reached ({}), skipping", self.config.max_positions);
            return Ok(());
        }

        // Check if already have position in this symbol
        if positions.values().any(|p| p.symbol == signal.symbol) {
            debug!("Already have position in {}, skipping", signal.symbol);
            return Ok(());
        }
        drop(positions);

        // Check cooldown
        if self.in_cooldown(&signal.symbol).await {
            debug!("{} in cooldown, skipping", signal.symbol);
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
            info!("   → Expected payout: $1.00 (profit: {:.0}¢ per share)",
                (dec!(1) - signal.pm_price) * dec!(100));
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
                format!(" → Expected: ${:.2}", profit_per_share * Decimal::from(self.config.shares_per_trade))
            } else {
                String::new()
            };
            info!("[DRY RUN] Would buy {} shares of {} {}{}",
                self.config.shares_per_trade, signal.symbol, signal.direction, expected_profit);
        } else {
            // Create and execute order
            let order = OrderRequest::buy_limit(
                token_id.clone(),
                signal.direction.into(),
                self.config.shares_per_trade,
                signal.pm_price,
            );

            match self.executor.execute(&order).await {
                Ok(result) => {
                    let fill_price = result.avg_fill_price.unwrap_or(signal.pm_price);
                    let trade_count = self.record_trade().await;
                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        result.filled_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Track position
                    let position = Position {
                        token_id: token_id.clone(),
                        symbol: signal.symbol.clone(),
                        direction: signal.direction,
                        entry_price: fill_price,
                        shares: result.filled_shares,
                        entry_time: Utc::now(),
                        highest_price: fill_price,
                        event_end_time: event.end_time,
                        event_slug: event.slug.clone(),
                    };

                    let mut positions = self.positions.write().await;
                    positions.insert(signal.symbol.clone(), position);
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        // Update last trade time
        let mut last_trade = self.last_trade_time.write().await;
        last_trade.insert(signal.symbol, Utc::now());

        Ok(())
    }

    /// Execute position exit
    async fn execute_exit(&self, symbol: &str, price: Decimal, reason: ExitReason) -> Result<()> {
        let mut positions = self.positions.write().await;

        let position = match positions.remove(symbol) {
            Some(p) => p,
            None => return Ok(()),
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
                }
                Err(e) => {
                    error!("Exit order failed: {}", e);
                    // Re-add position on failure
                    positions.insert(symbol.to_string(), position);
                }
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
            cex_move_pct: dec!(0.01),  // 1% (>= min_move_pct of 0.3%)
            pm_price: dec!(0.30),       // 30¢ (<= max_entry_price of 35¢)
            edge: dec!(0.10),           // 10% (>= min_edge of 3%)
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
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
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
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
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
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
        };

        // 20% loss should trigger stop loss
        let exit = manager.check_exit(&pos, dec!(0.40));
        assert!(matches!(exit, Some(ExitReason::StopLoss { .. })));
    }
}
