use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::super::types::Domain;

/// 風控配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskConfig {
    /// 平台最大總暴露 (USD)
    pub max_platform_exposure: Decimal,
    /// 最大連續失敗次數 (熔斷)
    pub max_consecutive_failures: u32,
    /// 每日最大損失 (USD)
    pub daily_loss_limit: Decimal,
    /// Optional hard drawdown stop (USD, absolute).
    pub max_drawdown_limit: Option<Decimal>,
    /// 最大點差 (basis points)
    pub max_spread_bps: u32,
    /// 緊急訂單是否跳過部分檢查
    pub critical_bypass_exposure: bool,
    /// Enable automatic circuit-breaker recovery after cooldown.
    #[serde(default = "default_circuit_breaker_auto_recover")]
    pub circuit_breaker_auto_recover: bool,
    /// Cooldown before auto-recovering from HALTED state.
    #[serde(default = "default_circuit_breaker_cooldown_secs")]
    pub circuit_breaker_cooldown_secs: u64,
    /// Optional per-domain exposure caps (USD)
    pub crypto_max_exposure: Option<Decimal>,
    pub sports_max_exposure: Option<Decimal>,
    pub politics_max_exposure: Option<Decimal>,
    pub economics_max_exposure: Option<Decimal>,
    /// Optional per-domain daily loss limits (USD)
    pub crypto_daily_loss_limit: Option<Decimal>,
    pub sports_daily_loss_limit: Option<Decimal>,
    pub politics_daily_loss_limit: Option<Decimal>,
    pub economics_daily_loss_limit: Option<Decimal>,
}

fn default_circuit_breaker_auto_recover() -> bool {
    true
}

fn default_circuit_breaker_cooldown_secs() -> u64 {
    300
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_platform_exposure: Decimal::from(5000),
            max_consecutive_failures: 5,
            daily_loss_limit: Decimal::from(1000),
            max_drawdown_limit: None,
            max_spread_bps: 500,
            critical_bypass_exposure: false,
            circuit_breaker_auto_recover: default_circuit_breaker_auto_recover(),
            circuit_breaker_cooldown_secs: default_circuit_breaker_cooldown_secs(),
            crypto_max_exposure: None,
            sports_max_exposure: None,
            politics_max_exposure: None,
            economics_max_exposure: None,
            crypto_daily_loss_limit: None,
            sports_daily_loss_limit: None,
            politics_daily_loss_limit: None,
            economics_daily_loss_limit: None,
        }
    }
}

impl RiskConfig {
    pub(super) fn domain_exposure_limit(&self, domain: Domain) -> Option<Decimal> {
        match domain {
            Domain::Crypto => self.crypto_max_exposure,
            Domain::Sports => self.sports_max_exposure,
            Domain::Politics => self.politics_max_exposure,
            Domain::Economics => self.economics_max_exposure,
            Domain::Custom(_) => None,
        }
    }

    pub(super) fn domain_daily_loss_limit(&self, domain: Domain) -> Option<Decimal> {
        match domain {
            Domain::Crypto => self.crypto_daily_loss_limit,
            Domain::Sports => self.sports_daily_loss_limit,
            Domain::Politics => self.politics_daily_loss_limit,
            Domain::Economics => self.economics_daily_loss_limit,
            Domain::Custom(_) => None,
        }
    }
}
