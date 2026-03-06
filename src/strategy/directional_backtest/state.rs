use chrono::{DateTime, Utc};
use rust_decimal::Decimal;

use crate::strategy::momentum::Direction;

#[derive(Debug, Clone)]
pub(super) struct DirectionalPosition {
    pub(super) symbol: String,
    pub(super) direction: Direction,
    pub(super) entry_price: Decimal,
    pub(super) entry_time: DateTime<Utc>,
    pub(super) shares: u64,
    #[allow(dead_code)]
    pub(super) event_slug: String,
    /// Window open price (Binance proxy for Chainlink S0)
    pub(super) s0: Decimal,
    /// When the event window settles
    pub(super) event_end_time: DateTime<Utc>,
    /// Model probability at entry
    pub(super) entry_p_hat: f64,
    /// EV_net at entry for diagnostics
    pub(super) entry_ev_net: f64,
    /// Realized vol at entry
    pub(super) entry_sigma: f64,
    /// Latest PM price for mark-to-market
    pub(super) latest_pm_price: Decimal,
}

#[derive(Debug, Clone)]
pub(super) struct ActiveWindowInfo {
    pub(super) event_slug: String,
    /// S0 = price_to_beat from EventState
    pub(super) s0: Decimal,
    pub(super) end_time: DateTime<Utc>,
}
