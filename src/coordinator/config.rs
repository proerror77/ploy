//! Coordinator Configuration

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::platform::RiskConfig;

/// Configuration for the coordinator
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        }
    }
}
