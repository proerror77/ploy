//! Coordinator Configuration

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::platform::RiskConfig;

/// Scope for duplicate-intent guard.
///
/// - `market`: block repeated BUY intents for the same (domain, market_slug) within the guard window,
///   regardless of which strategy deployment produced them. This is safer when multiple strategies
///   can overlap and would otherwise double-enter the same event.
/// - `deployment`: legacy behavior; scope duplicates by deployment_id (or agent+strategy fallback).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DuplicateGuardScope {
    Market,
    Deployment,
}

impl Default for DuplicateGuardScope {
    fn default() -> Self {
        Self::Market
    }
}

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
    /// Duplicate-intent guard scope (market vs deployment).
    pub duplicate_guard_scope: DuplicateGuardScope,
    /// Cooldown in seconds between repeated stale heartbeat warnings for same agent.
    pub heartbeat_stale_warn_cooldown_secs: u64,
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
    /// Enable/disable sports capital allocator.
    pub sports_allocator_enabled: bool,
    /// Total sports capital cap (USD). If None, falls back to risk caps.
    pub sports_allocator_total_cap_usd: Option<Decimal>,
    /// Per-market cap as percentage of total sports cap.
    pub sports_market_cap_pct: Decimal,
    /// If true, auto-split sports cap by active market count.
    pub sports_auto_split_by_active_markets: bool,

    // === Sizing policy (Coordinator-level) ===
    /// Enable Kelly-based sizing for buy intents when a strategy provides `signal_fair_value`.
    ///
    /// This is applied in the coordinator before risk checks and capital allocation.
    pub kelly_sizing_enabled: bool,
    /// Conservative Kelly multiplier (e.g., 0.25 = quarter-Kelly).
    pub kelly_fraction_multiplier: Decimal,
    /// Optional minimum edge (p - price) required to allow sizing; set 0 to disable.
    pub kelly_min_edge: Decimal,
}

impl Default for CoordinatorConfig {
    fn default() -> Self {
        Self {
            risk: RiskConfig::default(),
            state_refresh_ms: 1000,
            queue_drain_ms: 200,
            max_platform_exposure: None,
            heartbeat_timeout_ms: 30_000,
            batch_size: 10,
            duplicate_guard_window_ms: 60_000,
            duplicate_guard_enabled: true,
            duplicate_guard_scope: DuplicateGuardScope::Market,
            heartbeat_stale_warn_cooldown_secs: 300,
            crypto_allocator_enabled: true,
            crypto_allocator_total_cap_usd: None,
            // Conservative baseline allocator (can be overridden by env).
            // Bias exposure toward 15m markets and cap short-horizon 5m risk.
            crypto_coin_cap_btc_pct: Decimal::new(40, 2), // 40%
            crypto_coin_cap_eth_pct: Decimal::new(30, 2), // 30%
            crypto_coin_cap_sol_pct: Decimal::new(20, 2), // 20%
            crypto_coin_cap_xrp_pct: Decimal::new(12, 2), // 12%
            crypto_coin_cap_other_pct: Decimal::new(10, 2), // 10%
            crypto_horizon_cap_5m_pct: Decimal::new(25, 2), // 25%
            crypto_horizon_cap_15m_pct: Decimal::new(65, 2), // 65%
            crypto_horizon_cap_other_pct: Decimal::new(20, 2), // 20%
            sports_allocator_enabled: true,
            sports_allocator_total_cap_usd: None,
            sports_market_cap_pct: Decimal::new(35, 2), // 35%
            sports_auto_split_by_active_markets: true,

            // Kelly sizing is opt-in by env to preserve legacy behavior.
            kelly_sizing_enabled: false,
            kelly_fraction_multiplier: Decimal::new(25, 2), // 0.25 (quarter-Kelly)
            kelly_min_edge: Decimal::ZERO,
        }
    }
}
