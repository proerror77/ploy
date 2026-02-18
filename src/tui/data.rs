//! Data models for the TUI dashboard
//!
//! These models are optimized for display and derived from domain types.

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

use crate::domain::Side;

/// Position display data
#[derive(Debug, Clone)]
pub struct DisplayPosition {
    /// Position side (UP or DOWN)
    pub side: Side,
    /// Number of shares held
    pub shares: u64,
    /// Current market price
    pub current_price: Decimal,
    /// Unrealized PnL in USD
    pub pnl: Decimal,
    /// Total cost basis
    pub cost: Decimal,
    /// Average entry price
    pub avg_price: Decimal,
}

impl DisplayPosition {
    /// Create a new display position
    pub fn new(side: Side, shares: u64, current_price: Decimal, avg_price: Decimal) -> Self {
        let cost = avg_price * Decimal::from(shares);
        let current_value = current_price * Decimal::from(shares);
        let pnl = current_value - cost;

        Self {
            side,
            shares,
            current_price,
            pnl,
            cost,
            avg_price,
        }
    }

    /// Calculate PnL percentage
    pub fn pnl_pct(&self) -> Decimal {
        if self.cost.is_zero() {
            Decimal::ZERO
        } else {
            (self.pnl / self.cost) * dec!(100)
        }
    }

    /// Get progress ratio (0.0 to 1.0) based on current price
    pub fn progress_ratio(&self) -> f64 {
        self.current_price.to_string().parse::<f64>().unwrap_or(0.5)
    }
}

/// Market analysis display data
#[derive(Debug, Clone, Default)]
pub struct MarketState {
    /// UP side best ask price
    pub up_price: Decimal,
    /// DOWN side best ask price
    pub down_price: Decimal,
    /// Combined price (up + down)
    pub combined: Decimal,
    /// Spread percentage from 1.0
    pub spread_pct: Decimal,
    /// Number of paired shares
    pub pairs: u64,
    /// Delta between UP and DOWN positions
    pub delta: i64,
    /// Total realized + unrealized PnL
    pub total_pnl: Decimal,
    /// UP bid price
    pub up_bid: Decimal,
    /// DOWN bid price
    pub down_bid: Decimal,
    /// UP bid size
    pub up_size: Decimal,
    /// DOWN bid size
    pub down_size: Decimal,
}

impl MarketState {
    /// Create from quotes
    pub fn from_quotes(
        up_bid: Decimal,
        up_ask: Decimal,
        down_bid: Decimal,
        down_ask: Decimal,
        up_size: Decimal,
        down_size: Decimal,
    ) -> Self {
        let combined = up_ask + down_ask;
        let spread_pct = (combined - dec!(1)) * dec!(100);

        Self {
            up_price: up_ask,
            down_price: down_ask,
            combined,
            spread_pct,
            pairs: 0,
            delta: 0,
            total_pnl: Decimal::ZERO,
            up_bid,
            down_bid,
            up_size,
            down_size,
        }
    }

    /// Update with position data
    pub fn with_positions(&mut self, up_shares: u64, down_shares: u64, pnl: Decimal) {
        self.pairs = up_shares.min(down_shares);
        self.delta = up_shares as i64 - down_shares as i64;
        self.total_pnl = pnl;
    }
}

/// Transaction display data
#[derive(Debug, Clone)]
pub struct DisplayTransaction {
    /// Transaction timestamp
    pub time: DateTime<Utc>,
    /// Transaction side (UP or DOWN)
    pub side: Side,
    /// Fill price
    pub price: Decimal,
    /// Fill size in shares
    pub size: u64,
    /// BTC price at time of transaction
    pub btc_price: Decimal,
    /// Transaction hash (truncated for display)
    pub tx_hash: String,
}

impl DisplayTransaction {
    /// Create a new display transaction
    pub fn new(
        time: DateTime<Utc>,
        side: Side,
        price: Decimal,
        size: u64,
        btc_price: Decimal,
        tx_hash: String,
    ) -> Self {
        Self {
            time,
            side,
            price,
            size,
            btc_price,
            tx_hash,
        }
    }

    /// Get truncated tx hash for display
    pub fn short_hash(&self) -> String {
        if self.tx_hash.len() > 12 {
            format!("{}...", &self.tx_hash[..12])
        } else {
            self.tx_hash.clone()
        }
    }

    /// Format time for display (HH:MM:SS.ss)
    pub fn formatted_time(&self) -> String {
        self.time.format("%H:%M:%S%.2f").to_string()
    }
}

/// Dashboard statistics
#[derive(Debug, Clone, Default)]
pub struct DashboardStats {
    /// Total trade count
    pub trade_count: u64,
    /// Total volume in USD
    pub volume: Decimal,
    /// Round end time (for countdown)
    pub round_end_time: Option<DateTime<Utc>>,
    /// Current strategy state
    pub strategy_state: String,
    /// Is dry run mode
    pub dry_run: bool,
    /// Latest Binance price (BTC/SOL/ETH)
    pub binance_price: Option<Decimal>,
    /// Binance symbol being tracked (e.g. "BTCUSDT")
    pub binance_symbol: String,
    /// WebSocket connection status
    pub ws_connected: bool,
    /// Last error message
    pub last_error: Option<String>,
}

/// Agent display data for the coordinator TUI panel
#[derive(Debug, Clone)]
pub struct DisplayAgent {
    pub agent_id: String,
    pub name: String,
    pub domain: String,
    pub status: String,
    pub position_count: usize,
    pub exposure: Decimal,
    pub daily_pnl: Decimal,
    pub last_heartbeat: String,
    pub is_healthy: bool,
}

impl DisplayAgent {
    /// Create from a coordinator AgentSnapshot
    pub fn from_snapshot(snap: &crate::coordinator::AgentSnapshot) -> Self {
        Self {
            agent_id: snap.agent_id.clone(),
            name: snap.name.clone(),
            domain: format!("{:?}", snap.domain),
            status: format!("{:?}", snap.status),
            position_count: snap.position_count,
            exposure: snap.exposure,
            daily_pnl: snap.daily_pnl,
            last_heartbeat: snap.last_heartbeat.format("%H:%M:%S").to_string(),
            is_healthy: snap.error_message.is_none(),
        }
    }
}

/// Risk state display data for the TUI risk widget
#[derive(Debug, Clone)]
pub struct DisplayRiskState {
    /// Current platform risk state (Normal/Elevated/Halted)
    pub state: String,
    /// Daily loss used so far
    pub daily_loss_used: Decimal,
    /// Daily loss limit
    pub daily_loss_limit: Decimal,
    /// Queue depth (pending orders)
    pub queue_depth: usize,
    /// Circuit breaker state label
    pub circuit_breaker: String,
    /// Total position exposure in USD
    pub total_exposure: Decimal,
}

impl Default for DisplayRiskState {
    fn default() -> Self {
        Self {
            state: "Normal".to_string(),
            daily_loss_used: Decimal::ZERO,
            daily_loss_limit: Decimal::from(500),
            queue_depth: 0,
            circuit_breaker: "Closed".to_string(),
            total_exposure: Decimal::ZERO,
        }
    }
}

impl DashboardStats {
    /// Get remaining time until round end
    pub fn time_remaining(&self) -> Option<i64> {
        self.round_end_time
            .map(|end| (end - Utc::now()).num_seconds().max(0))
    }

    /// Format remaining time as MM:SS
    pub fn formatted_remaining(&self) -> String {
        match self.time_remaining() {
            Some(secs) => {
                let mins = secs / 60;
                let secs = secs % 60;
                format!("{:02}:{:02}", mins, secs)
            }
            None => "--:--".to_string(),
        }
    }
}
