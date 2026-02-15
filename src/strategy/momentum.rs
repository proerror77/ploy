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
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

use crate::adapters::{
    GammaEventInfo, PolymarketClient, PriceCache, PriceUpdate, QuoteCache, QuoteUpdate, SpotPrice,
};
use crate::config::RiskConfig;
use crate::domain::{OrderRequest, Side};
use crate::error::Result;
use crate::strategy::dump_hedge::{DumpHedgeConfig, DumpHedgeEngine};
use crate::strategy::fund_manager::{FundManager, PositionSizeResult};
use crate::strategy::volatility::{EventTracker, VolatilityConfig, VolatilityDetector};
use crate::strategy::OrderExecutor;

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

            // === VWAP CONFIRMATION (DEFAULT: OFF) ===
            require_vwap_confirmation: false,
            vwap_lookback_secs: 60,
            min_vwap_deviation: dec!(0),
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
    /// The price threshold that determines UP/DOWN outcome
    /// Parsed from market question like "Will BTC be above $94,000?"
    pub price_to_beat: Option<Decimal>,
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

    /// Parse price threshold from market question
    /// Examples:
    /// - "Will BTC be above $94,000 at 9:15 PM?" → 94000
    /// - "Will ETH be above $3,500.50 at 10:00 AM?" → 3500.50
    /// - "↑ 94,000" → 94000
    pub fn parse_price_from_question(question: &str) -> Option<Decimal> {
        // Try to find price pattern: $X,XXX or $X,XXX.XX or just numbers with commas
        // Pattern: optional $ followed by digits with optional commas and decimal
        let cleaned: String = question
            .chars()
            .skip_while(|c| !c.is_ascii_digit() && *c != '$')
            .skip_while(|c| *c == '$')
            .take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.')
            .filter(|c| *c != ',')
            .collect();

        if cleaned.is_empty() {
            return None;
        }

        Decimal::from_str(&cleaned).ok()
    }
}

impl EventMatcher {
    /// Create a new event matcher
    pub fn new(client: PolymarketClient) -> Self {
        let mut symbol_to_series = HashMap::new();

        // Map each symbol to its series IDs (from Gamma API)
        // Prefer 5m first, then 15m fallback.

        // BTC: 10684 = 5m, 10192 = 15m
        symbol_to_series.insert(
            "BTCUSDT".into(),
            vec![
                "10684".into(), // btc-up-or-down-5m
                "10192".into(), // btc-up-or-down-15m
            ],
        );

        // ETH: 10683 = 5m, 10191 = 15m
        symbol_to_series.insert(
            "ETHUSDT".into(),
            vec![
                "10683".into(), // eth-up-or-down-5m
                "10191".into(), // eth-up-or-down-15m
            ],
        );

        // SOL: 10686 = 5m, 10423 = 15m
        symbol_to_series.insert(
            "SOLUSDT".into(),
            vec![
                "10686".into(), // sol-up-or-down-5m
                "10423".into(), // sol-up-or-down-15m
            ],
        );

        // XRP: 10685 = 5m, 10422 = 15m
        symbol_to_series.insert(
            "XRPUSDT".into(),
            vec![
                "10685".into(), // xrp-up-or-down-5m
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
        self.find_event_with_timing(symbol, 60, i64::MAX, false)
            .await
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
        let mut best: Option<(i64, EventInfo)> = None;

        // Search through all series for this symbol and choose the global best.
        for series_id in series_ids {
            let Some(series_events) = events.get(series_id) else {
                continue;
            };

            for event in series_events {
                let remaining = event.time_remaining().num_seconds();
                if remaining < min_secs as i64 || remaining > max_secs {
                    continue;
                }

                let is_better = match best.as_ref() {
                    None => true,
                    Some((best_remaining, _)) => {
                        if prefer_close_to_end {
                            remaining < *best_remaining
                        } else {
                            remaining > *best_remaining
                        }
                    }
                };

                if is_better {
                    best = Some((remaining, event.clone()));
                }
            }
        }

        best.map(|(_, event)| event)
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
        let now = Utc::now();

        // Filter to events ending within the next 60 minutes
        // This gives us a buffer to catch upcoming 15-minute windows
        // The trading logic will filter based on max_time_remaining_secs for entry timing
        let max_end_time = now + ChronoDuration::minutes(60);
        let min_end_time = now + ChronoDuration::seconds(30); // At least 30s remaining

        let mut sorted_events: Vec<_> = gamma_events
            .into_iter()
            .filter(|e| {
                // Parse end_date from gamma event to filter by time
                if let Some(end_str) = &e.end_date {
                    if let Ok(end) = DateTime::parse_from_rfc3339(end_str) {
                        let end_utc = end.with_timezone(&Utc);
                        // Only keep events ending within our window
                        return end_utc > min_end_time && end_utc <= max_end_time;
                    }
                }
                false
            })
            .collect();

        // Sort by end_time (soonest first)
        sorted_events.sort_by(|a, b| {
            let a_end = a
                .end_date
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
            let b_end = b
                .end_date
                .as_ref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok());
            a_end.cmp(&b_end)
        });

        info!(
            "Series {}: {} events ending in next 60 minutes",
            series_id,
            sorted_events.len()
        );

        let mut events = vec![];
        // Take up to 5 events that end soonest
        for gamma_event in sorted_events.into_iter().take(5) {
            // Get full event details to access market condition_id
            let event_details = match self.client.get_event_details(&gamma_event.id).await {
                Ok(details) => details,
                Err(e) => {
                    debug!("Failed to get details for event {}: {}", gamma_event.id, e);
                    continue;
                }
            };

            // Get market from event details
            let market = match event_details.markets.first() {
                Some(m) => m,
                None => continue,
            };
            let condition_id = market.condition_id.clone().unwrap_or_default();

            // Parse token IDs from clobTokenIds field (JSON string array)
            // Format: ["token_id_for_up", "token_id_for_down"]
            let tokens: Vec<String> = market
                .clob_token_ids
                .as_ref()
                .and_then(|ids_str| serde_json::from_str::<Vec<String>>(ids_str).ok())
                .unwrap_or_default();

            if tokens.len() < 2 {
                debug!(
                    "Market {} has insufficient tokens: {:?}",
                    condition_id, tokens
                );
                continue;
            }

            // First token is UP, second is DOWN (Polymarket convention)
            let up_token_id = tokens[0].clone();
            let down_token_id = tokens[1].clone();

            // Parse end time
            let end_time = match event_details.end_date.as_ref().and_then(|s| {
                DateTime::parse_from_rfc3339(s)
                    .map(|dt| dt.with_timezone(&Utc))
                    .ok()
            }) {
                Some(t) => t,
                None => continue,
            };

            // Parse price_to_beat from market title (e.g., "Will BTC be above $94,000?")
            let price_to_beat = EventInfo::parse_price_from_question(
                &event_details.title.clone().unwrap_or_default(),
            );

            let event_info = EventInfo {
                slug: event_details.slug.clone().unwrap_or_default(),
                title: event_details.title.clone().unwrap_or_default(),
                up_token_id,
                down_token_id,
                end_time,
                condition_id,
                price_to_beat,
            };

            debug!(
                "Found event: {} (UP={}, DOWN={})",
                event_info.title,
                &event_info.up_token_id[..20.min(event_info.up_token_id.len())],
                &event_info.down_token_id[..20.min(event_info.down_token_id.len())]
            );

            events.push(event_info);
        }

        Ok(events)
    }

    /// Convert Gamma API event to EventInfo
    fn convert_gamma_event(&self, gamma: &GammaEventInfo) -> Option<EventInfo> {
        debug!(
            "Converting event: id={}, markets={}",
            gamma.id,
            gamma.markets.len()
        );

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

        let title = gamma.title.clone().unwrap_or_default();
        let price_to_beat = EventInfo::parse_price_from_question(&title);

        Some(EventInfo {
            slug: gamma.slug.clone().unwrap_or_default(),
            title,
            up_token_id: up_token.token_id.clone(),
            down_token_id: down_token.token_id.clone(),
            end_time,
            condition_id: market.condition_id.clone().unwrap_or_default(),
            price_to_beat,
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

    /// Get token IDs with their corresponding sides for WebSocket registration
    pub async fn get_token_mappings(&self) -> Vec<(String, Side)> {
        let events = self.active_events.read().await;
        let mut mappings = vec![];

        for series_events in events.values() {
            for event in series_events {
                mappings.push((event.up_token_id.clone(), Side::Up));
                mappings.push((event.down_token_id.clone(), Side::Down));
            }
        }

        mappings
    }

    /// Get reference to the Polymarket client
    pub fn client(&self) -> &PolymarketClient {
        &self.client
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
    /// Cached K-line volatility per symbol
    kline_volatility: HashMap<String, Decimal>,
}

impl MomentumDetector {
    pub fn new(config: MomentumConfig) -> Self {
        Self {
            config,
            kline_volatility: HashMap::new(),
        }
    }

    /// Update K-line volatility for a symbol
    pub fn update_kline_volatility(&mut self, symbol: &str, volatility: Decimal) {
        self.kline_volatility.insert(symbol.to_string(), volatility);
    }

    /// Check for momentum signal given CEX and PM prices
    /// Uses weighted momentum (10s/30s/60s) and volatility-adjusted thresholds
    pub fn check(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
    ) -> Option<MomentumSignal> {
        // Calculate CEX momentum (weighted or single timeframe)
        // Always use weighted multi-timeframe momentum (10s/30s/60s),
        // falling back to single-timeframe if insufficient history
        let momentum = match spot.weighted_momentum() {
            Some(m) => m,
            None => {
                debug!(
                    "{} insufficient history for weighted momentum, using single timeframe",
                    symbol
                );
                spot.momentum(self.config.lookback_secs)?
            }
        };

        // Calculate volatility-adjusted threshold
        let effective_threshold = self.calculate_effective_threshold(symbol, spot);

        // Log momentum and threshold for debugging
        debug!(
            "{} weighted_momentum={:.4}% threshold={:.4}% (vol_adjusted={})",
            symbol,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            self.config.use_volatility_adjustment
        );

        // Check minimum move threshold
        if momentum.abs() < effective_threshold {
            return None;
        }

        // Determine direction and PM price
        let (direction, pm_price) = if momentum > Decimal::ZERO {
            (Direction::Up, up_ask?)
        } else {
            (Direction::Down, down_ask?)
        };

        // Optional VWAP confirmation (Binance trade-volume VWAP)
        if self.config.require_vwap_confirmation {
            let vwap = match spot.vwap(self.config.vwap_lookback_secs) {
                Some(v) => v,
                None => {
                    debug!(
                        "{} {} insufficient data for VWAP confirmation (lookback={}s)",
                        symbol, direction, self.config.vwap_lookback_secs
                    );
                    return None;
                }
            };

            let dev = self.config.min_vwap_deviation.max(dec!(0));
            let ok = match direction {
                Direction::Up => spot.price >= vwap * (Decimal::ONE + dev),
                Direction::Down => spot.price <= vwap * (Decimal::ONE - dev),
            };

            if !ok {
                debug!(
                    "{} {} VWAP confirmation failed: spot=${:.4} vwap=${:.4} dev={:.3}%",
                    symbol,
                    direction,
                    spot.price,
                    vwap,
                    dev * dec!(100)
                );
                return None;
            }
        }

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

        info!(
            "🎯 SIGNAL: {} {} | momentum={:.3}% threshold={:.3}% | PM={:.1}¢ edge={:.1}%",
            symbol,
            direction,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            pm_price * dec!(100),
            edge * dec!(100)
        );

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

    /// Calculate effective threshold based on current volatility
    /// threshold = base_threshold * (current_vol / baseline_vol)
    fn calculate_effective_threshold(&self, symbol: &str, spot: &SpotPrice) -> Decimal {
        if !self.config.use_volatility_adjustment {
            return self.config.min_move_pct;
        }

        // Get baseline volatility for this symbol
        let baseline_vol = self
            .config
            .baseline_volatility
            .get(symbol)
            .copied()
            .unwrap_or(dec!(0.001)); // Default 0.1% if not configured

        // Calculate current volatility
        let current_vol = spot
            .volatility(self.config.volatility_lookback_secs)
            .unwrap_or(baseline_vol);

        // Avoid division by zero
        if baseline_vol.is_zero() {
            return self.config.min_move_pct;
        }

        // Adjust threshold: higher vol = higher threshold, lower vol = lower threshold
        // But clamp to reasonable range (0.5x to 2x base threshold)
        let vol_ratio = current_vol / baseline_vol;
        let clamped_ratio = vol_ratio.max(dec!(0.5)).min(dec!(2.0));

        let adjusted = self.config.min_move_pct * clamped_ratio;

        debug!(
            "{} vol_adjust: current={:.4}% baseline={:.4}% ratio={:.2} threshold={:.4}%",
            symbol,
            current_vol * dec!(100),
            baseline_vol * dec!(100),
            clamped_ratio,
            adjusted * dec!(100)
        );

        adjusted
    }

    /// Estimate fair value based on CEX momentum
    fn estimate_fair_value(&self, momentum: Decimal) -> Decimal {
        // Improved model: uses sigmoid-like scaling
        // Small moves have modest impact, large moves have bigger impact
        let base_prob = dec!(0.50);

        // Scale: 0.1% move → ~5% probability shift
        //        0.5% move → ~20% probability shift
        //        1.0% move → ~35% probability shift
        let abs_momentum = momentum.abs();
        let momentum_factor = if abs_momentum < dec!(0.001) {
            // Very small moves: linear scaling
            abs_momentum * dec!(50) // 0.1% → 5%
        } else if abs_momentum < dec!(0.005) {
            // Medium moves: moderate scaling
            dec!(0.05) + (abs_momentum - dec!(0.001)) * dec!(40) // 0.5% → ~21%
        } else {
            // Large moves: diminishing returns
            dec!(0.21) + (abs_momentum - dec!(0.005)) * dec!(30) // 1% → ~36%
        };

        // Cap at 90% probability
        (base_prob + momentum_factor).min(dec!(0.90))
    }

    /// Calculate confidence score (0.0 to 1.0)
    fn calculate_confidence(&self, momentum: Decimal, edge: Decimal) -> f64 {
        // Higher momentum and edge = higher confidence
        let momentum_score = (momentum.abs() / dec!(0.005)).min(Decimal::ONE); // 0.5% = max score
        let edge_score = (edge / dec!(0.15)).min(Decimal::ONE);

        // Weighted average
        let score = momentum_score * dec!(0.4) + edge_score * dec!(0.6);

        // Convert to f64, clamp to [0, 1]
        score
            .to_string()
            .parse::<f64>()
            .unwrap_or(0.5)
            .clamp(0.0, 1.0)
    }

    // ========================================================================
    // ENHANCED CHECK METHOD
    // ========================================================================

    /// Enhanced momentum check with all optimizations
    ///
    /// Includes:
    /// - Multi-timeframe agreement check
    /// - OBI confirmation
    /// - K-line volatility
    /// - Time decay
    /// - Price-to-beat consideration
    pub fn check_enhanced(
        &self,
        symbol: &str,
        spot: &SpotPrice,
        up_ask: Option<Decimal>,
        down_ask: Option<Decimal>,
        obi: Option<Decimal>,
        time_remaining_secs: i64,
        price_to_beat: Option<Decimal>,
    ) -> Option<MomentumSignal> {
        // 1. Multi-timeframe momentum check
        let (momentum, mtf_agrees) = self.check_multi_timeframe(spot);

        // If MTF agreement required but not met, skip
        if self.config.require_mtf_agreement && !mtf_agrees {
            debug!("{} MTF disagreement: timeframes not aligned", symbol);
            return None;
        }

        // 2. Calculate volatility-adjusted threshold
        let effective_threshold = if self.config.use_kline_volatility {
            self.calculate_kline_threshold(symbol, spot)
        } else {
            self.calculate_effective_threshold(symbol, spot)
        };

        // Check minimum move
        if momentum.abs() < effective_threshold {
            return None;
        }

        // 3. Determine direction
        let direction = if momentum > Decimal::ZERO {
            Direction::Up
        } else {
            Direction::Down
        };

        // 4. OBI confirmation check
        if self.config.min_obi_confirmation > Decimal::ZERO {
            if let Some(obi_val) = obi {
                let obi_confirms = match direction {
                    Direction::Up => obi_val >= self.config.min_obi_confirmation,
                    Direction::Down => obi_val <= -self.config.min_obi_confirmation,
                };

                if !obi_confirms {
                    debug!(
                        "{} OBI {:.2} does not confirm {} direction",
                        symbol, obi_val, direction
                    );
                    return None;
                }
            }
        }

        // 5. Get PM price
        let pm_price = match direction {
            Direction::Up => up_ask?,
            Direction::Down => down_ask?,
        };

        // Check max entry price
        if pm_price > self.config.max_entry_price {
            return None;
        }

        // 6. Calculate enhanced fair value with price-to-beat
        let fair_value = if self.config.use_price_to_beat {
            self.estimate_fair_value_with_price_to_beat(
                momentum,
                spot.price,
                price_to_beat,
                time_remaining_secs,
            )
        } else {
            self.estimate_fair_value(momentum)
        };

        // 7. Apply time decay
        let time_adjusted_fair_value = if self.config.time_decay_factor > Decimal::ZERO {
            let time_factor = Decimal::from(time_remaining_secs.max(0)) / dec!(900);
            let decay = dec!(1) - (self.config.time_decay_factor * (dec!(1) - time_factor));
            // Interpolate between base (0.5) and fair_value
            let base = dec!(0.5);
            base + (fair_value - base) * decay
        } else {
            fair_value
        };

        let edge = time_adjusted_fair_value - pm_price;

        if edge < self.config.min_edge {
            debug!(
                "{} {} edge {:.2}% < min {:.2}%",
                symbol,
                direction,
                edge * dec!(100),
                self.config.min_edge * dec!(100)
            );
            return None;
        }

        // 8. Enhanced confidence calculation
        let confidence = self.calculate_enhanced_confidence(
            momentum,
            edge,
            obi,
            mtf_agrees,
            time_remaining_secs,
        );

        // Check minimum confidence
        if confidence < self.config.min_confidence {
            debug!(
                "{} {} confidence {:.0}% < min {:.0}%",
                symbol,
                direction,
                confidence * 100.0,
                self.config.min_confidence * 100.0
            );
            return None;
        }

        info!(
            "🎯 ENHANCED SIGNAL: {} {} | mom={:.3}% thr={:.3}% | PM={:.1}¢ FV={:.1}¢ edge={:.1}% | conf={:.0}% | {}s left{}",
            symbol,
            direction,
            momentum * dec!(100),
            effective_threshold * dec!(100),
            pm_price * dec!(100),
            time_adjusted_fair_value * dec!(100),
            edge * dec!(100),
            confidence * 100.0,
            time_remaining_secs,
            if mtf_agrees { " [MTF✓]" } else { "" }
        );

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

    /// Check multi-timeframe momentum agreement
    /// Returns (weighted_momentum, all_agree)
    fn check_multi_timeframe(&self, spot: &SpotPrice) -> (Decimal, bool) {
        let mom_10s = spot.momentum(10);
        let mom_30s = spot.momentum(30);
        let mom_60s = spot.momentum(60);

        // Calculate weighted momentum
        let weighted = match (mom_10s, mom_30s, mom_60s) {
            (Some(m10), Some(m30), Some(m60)) => {
                m10 * dec!(0.2) + m30 * dec!(0.3) + m60 * dec!(0.5)
            }
            (Some(m10), Some(m30), None) => m10 * dec!(0.4) + m30 * dec!(0.6),
            (Some(m), _, _) | (_, Some(m), _) | (_, _, Some(m)) => m,
            _ => return (Decimal::ZERO, false),
        };

        // Check agreement (all same sign)
        let all_agree = match (mom_10s, mom_30s, mom_60s) {
            (Some(m10), Some(m30), Some(m60)) => {
                (m10 > Decimal::ZERO && m30 > Decimal::ZERO && m60 > Decimal::ZERO)
                    || (m10 < Decimal::ZERO && m30 < Decimal::ZERO && m60 < Decimal::ZERO)
            }
            (Some(m10), Some(m30), None) => {
                (m10 > Decimal::ZERO && m30 > Decimal::ZERO)
                    || (m10 < Decimal::ZERO && m30 < Decimal::ZERO)
            }
            _ => false,
        };

        (weighted, all_agree)
    }

    /// Calculate threshold using K-line historical volatility
    fn calculate_kline_threshold(&self, symbol: &str, spot: &SpotPrice) -> Decimal {
        // Try K-line volatility first
        let kline_vol = self.kline_volatility.get(symbol).copied();

        let current_vol = if let Some(vol) = kline_vol {
            vol
        } else {
            // Fall back to tick-based volatility
            spot.volatility(self.config.volatility_lookback_secs)
                .unwrap_or(dec!(0.001))
        };

        let baseline_vol = self
            .config
            .baseline_volatility
            .get(symbol)
            .copied()
            .unwrap_or(dec!(0.001));

        if baseline_vol.is_zero() {
            return self.config.min_move_pct;
        }

        let vol_ratio = (current_vol / baseline_vol).max(dec!(0.5)).min(dec!(2.0));
        self.config.min_move_pct * vol_ratio
    }

    /// Estimate fair value considering price-to-beat
    fn estimate_fair_value_with_price_to_beat(
        &self,
        momentum: Decimal,
        current_price: Decimal,
        price_to_beat: Option<Decimal>,
        time_remaining_secs: i64,
    ) -> Decimal {
        let base_fv = self.estimate_fair_value(momentum);

        let price_threshold = match price_to_beat {
            Some(p) => p,
            None => return base_fv,
        };

        // Calculate how far current price is from threshold
        let distance_pct = if price_threshold > Decimal::ZERO {
            (current_price - price_threshold) / price_threshold
        } else {
            return base_fv;
        };

        // Time factor: more confident as time runs out
        let time_factor = dec!(1) - (Decimal::from(time_remaining_secs.max(0)) / dec!(900));

        // If price is above threshold (UP likely):
        //   distance > 0 → boost fair value
        // If price is below threshold (DOWN likely):
        //   distance < 0 → boost fair value for DOWN
        let direction_matches = (momentum > Decimal::ZERO && distance_pct > Decimal::ZERO)
            || (momentum < Decimal::ZERO && distance_pct < Decimal::ZERO);

        if direction_matches {
            // Boost fair value: larger distance + less time = more confident
            let boost = distance_pct.abs() * time_factor * dec!(0.5);
            (base_fv + boost).min(dec!(0.95))
        } else {
            // Direction doesn't match price-to-beat, reduce fair value
            let reduction = distance_pct.abs() * dec!(0.3);
            (base_fv - reduction).max(dec!(0.35))
        }
    }

    /// Enhanced confidence calculation
    fn calculate_enhanced_confidence(
        &self,
        momentum: Decimal,
        edge: Decimal,
        obi: Option<Decimal>,
        mtf_agrees: bool,
        time_remaining_secs: i64,
    ) -> f64 {
        let mut score: f64 = 0.0;

        // Momentum contribution (0 - 0.25)
        let mom_score = (momentum.abs() / dec!(0.005)).min(Decimal::ONE);
        score += mom_score.to_string().parse::<f64>().unwrap_or(0.0) * 0.25;

        // Edge contribution (0 - 0.25)
        let edge_score = (edge / dec!(0.15)).min(Decimal::ONE);
        score += edge_score.to_string().parse::<f64>().unwrap_or(0.0) * 0.25;

        // OBI confirmation (0 - 0.15)
        if let Some(obi_val) = obi {
            let obi_strength = (obi_val.abs() / dec!(0.2)).min(Decimal::ONE);
            score += obi_strength.to_string().parse::<f64>().unwrap_or(0.0) * 0.15;
        }

        // MTF agreement (0 - 0.15)
        if mtf_agrees {
            score += 0.15;
        }

        // Time bonus (0 - 0.20): more confident with less time remaining
        let time_factor = 1.0 - (time_remaining_secs.max(0) as f64 / 900.0);
        score += time_factor * 0.20;

        score.clamp(0.0, 1.0)
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
    pub condition_id: String,
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
                write!(
                    f,
                    "TrailingStop(high={:.2}¢, cur={:.2}¢)",
                    high * dec!(100),
                    current * dec!(100)
                )
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
            claimer: None,
            trade_logger: None,
            window_tracker: Arc::new(RwLock::new(WindowRiskTracker::default())),
            lob_cache: None,
            kline_client: None,
            dump_hedge: None,
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
                        fm.record_position_closed(&pos.condition_id, &pos.symbol)
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

    /// Check if we won based on market outcome prices
    fn check_if_won(&self, pos: &Position, market: &crate::adapters::MarketResponse) -> bool {
        // Find our token in the market tokens
        for token in &market.tokens {
            if token.token_id == pos.token_id {
                // Parse the price - winner has price = 1.0
                if let Some(ref price_str) = token.price {
                    if let Ok(price) = price_str.parse::<f64>() {
                        return price > 0.5; // Winner = 1.0, Loser = 0.0
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
        mut binance_rx: broadcast::Receiver<PriceUpdate>,
        mut pm_rx: broadcast::Receiver<QuoteUpdate>,
        binance_cache: &PriceCache,
        pm_cache: &QuoteCache,
    ) -> Result<()> {
        info!("Starting momentum engine (dry_run={})", self.dry_run);

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
            if positions.contains_key(&event.condition_id) {
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

        // Determine shares to trade - use fund manager if available
        let shares_to_trade = if let Some(ref fm) = self.fund_manager {
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
                    let trade_count = self.record_trade().await;
                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        result.filled_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record position with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened(&event.condition_id, &signal.symbol)
                            .await;
                    }

                    // Track position in local state
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
                        condition_id: event.condition_id.clone(),
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
                                result.filled_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
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

        // Re-check if we already have position (might have changed since queueing)
        {
            let positions = self.positions.read().await;
            if positions.values().any(|p| p.symbol == signal.symbol) {
                debug!("Already have position in {}, skipping", signal.symbol);
                return Ok(());
            }
        }

        // Get position size
        let shares_to_trade = if let Some(ref fm) = self.fund_manager {
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
                    let trade_count = self.record_trade().await;

                    info!(
                        "Order filled: {} shares @ {:.2}¢ (trade #{} today)",
                        result.filled_shares,
                        fill_price * dec!(100),
                        trade_count
                    );

                    // Record with fund manager
                    if let Some(ref fm) = self.fund_manager {
                        fm.record_position_opened(&event.condition_id, &signal.symbol)
                            .await;
                    }

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
                        condition_id: event.condition_id.clone(),
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
                                result.filled_shares,
                                signal.cex_move_pct,
                                signal.edge,
                            )
                            .await;
                    }
                }
                Err(e) => {
                    error!("Order failed: {}", e);
                }
            }
        }

        // Update cooldown
        let mut last_trade = self.last_trade_time.write().await;
        last_trade.insert(signal.symbol.clone(), Utc::now());

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
            shares: 100,
            entry_time: Utc::now(),
            highest_price: dec!(0.50),
            event_end_time: Utc::now() + ChronoDuration::minutes(10),
            event_slug: "test".into(),
            condition_id: "test_condition".into(),
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
            condition_id: "test_condition".into(),
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
            condition_id: "test_condition".into(),
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
            end_time: now + ChronoDuration::seconds(seconds_remaining),
            condition_id: format!("{slug}-condition"),
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
