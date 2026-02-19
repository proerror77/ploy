//! Coordinator Configuration

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::platform::RiskConfig;

/// Configuration for the coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CoordinatorConfig {
    /// Risk configuration (reused from platform)
    pub risk: RiskConfig,
    /// How often to refresh GlobalState from aggregators (ms)
    pub state_refresh_ms: u64,
    /// How often to drain the order queue and execute (ms)
    pub queue_drain_ms: u64,
    /// Maximum platform-wide exposure (USD) — overrides risk config if set
    pub max_platform_exposure: Option<Decimal>,
    /// Maximum time without heartbeat before marking agent unhealthy (ms)
    pub heartbeat_timeout_ms: u64,
    /// Maximum orders to dequeue per drain cycle
    pub batch_size: usize,
    /// Block duplicate buy intents (same market slug) within this window.
    pub duplicate_guard_window_ms: u64,
    /// Enable/disable duplicate-intent guard.
    pub duplicate_guard_enabled: bool,
    /// Enable/disable crypto capital allocator.
    pub crypto_allocator_enabled: bool,
    /// Total crypto capital cap (USD). If None, falls back to risk caps.
    pub crypto_allocator_total_cap_usd: Option<Decimal>,
    /// Per-coin allocation caps as percentages of total crypto cap.
    pub crypto_coin_cap_btc_pct: Decimal,
    pub crypto_coin_cap_eth_pct: Decimal,
    pub crypto_coin_cap_sol_pct: Decimal,
    pub crypto_coin_cap_xrp_pct: Decimal,
    pub crypto_coin_cap_other_pct: Decimal,
    /// Per-horizon allocation caps as percentages of total crypto cap.
    pub crypto_horizon_cap_5m_pct: Decimal,
    pub crypto_horizon_cap_15m_pct: Decimal,
    pub crypto_horizon_cap_other_pct: Decimal,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            risk: RiskConfig::default(),
            state_refresh_ms: 1000,
            queue_drain_ms: 200,
            max_platform_exposure: None,
            heartbeat_timeout_ms: 15_000,
            batch_size: 10,
            duplicate_guard_window_ms: 15_000,
            duplicate_guard_enabled: true,
            crypto_allocator_enabled: true,
            crypto_allocator_total_cap_usd: None,
            crypto_coin_cap_btc_pct: Decimal::new(45, 2), // 45%
            crypto_coin_cap_eth_pct: Decimal::new(35, 2), // 35%
            crypto_coin_cap_sol_pct: Decimal::new(20, 2), // 20%
            crypto_coin_cap_xrp_pct: Decimal::new(15, 2), // 15%
            crypto_coin_cap_other_pct: Decimal::new(10, 2), // 10%
            crypto_horizon_cap_5m_pct: Decimal::new(50, 2), // 50%
            crypto_horizon_cap_15m_pct: Decimal::new(60, 2), // 60%
            crypto_horizon_cap_other_pct: Decimal::new(25, 2), // 25%
        }
    }
}
