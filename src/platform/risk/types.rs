use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::super::types::Domain;

/// 風控檢查結果
#[derive(Debug, Clone)]
pub enum RiskCheckResult {
    /// 通過
    Passed,
    /// 被攔截
    Blocked(BlockReason),
    /// 需要調整 (例如減少數量)
    Adjusted(AdjustmentSuggestion),
}

impl RiskCheckResult {
    pub fn is_passed(&self) -> bool {
        matches!(self, RiskCheckResult::Passed)
    }

    pub fn is_blocked(&self) -> bool {
        matches!(self, RiskCheckResult::Blocked(_))
    }
}

/// 攔截原因
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BlockReason {
    /// 熔斷觸發
    CircuitBreakerTripped { reason: String },
    /// 超過單筆限額
    ExceedsSingleLimit { limit: Decimal, requested: Decimal },
    /// 超過總暴露
    ExceedsTotalExposure {
        limit: Decimal,
        current: Decimal,
        requested: Decimal,
    },
    /// Domain exposure cap exceeded
    DomainExposureExceeded {
        domain: Domain,
        limit: Decimal,
        current: Decimal,
        requested: Decimal,
    },
    /// 每日損失超限
    DailyLossExceeded { limit: Decimal, current: Decimal },
    /// Domain daily loss cap exceeded
    DomainDailyLossExceeded {
        domain: Domain,
        limit: Decimal,
        current: Decimal,
    },
    /// Drawdown cap exceeded
    DrawdownExceeded { limit: Decimal, current: Decimal },
    /// 市場不允許
    MarketNotAllowed { market: String, agent: String },
    /// Agent 狀態不允許交易
    AgentNotActive { agent: String, status: String },
    /// Agent 未註冊風控參數
    UnregisteredAgent { agent: String },
    /// 訂單已過期
    OrderExpired,
    /// 未對沖倉位過多
    TooManyUnhedgedPositions { limit: u32, current: u32 },
}

impl std::fmt::Display for BlockReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockReason::CircuitBreakerTripped { reason } => {
                write!(f, "Circuit breaker: {}", reason)
            }
            BlockReason::ExceedsSingleLimit { limit, requested } => {
                write!(f, "Single order ${} exceeds limit ${}", requested, limit)
            }
            BlockReason::ExceedsTotalExposure {
                limit,
                current,
                requested,
            } => {
                write!(
                    f,
                    "Total exposure ${} + ${} exceeds ${}",
                    current, requested, limit
                )
            }
            BlockReason::DomainExposureExceeded {
                domain,
                limit,
                current,
                requested,
            } => {
                write!(
                    f,
                    "{} exposure ${} + ${} exceeds ${}",
                    domain, current, requested, limit
                )
            }
            BlockReason::DailyLossExceeded { limit, current } => {
                write!(f, "Daily loss ${} exceeds limit ${}", current, limit)
            }
            BlockReason::DomainDailyLossExceeded {
                domain,
                limit,
                current,
            } => {
                write!(
                    f,
                    "{} daily loss ${} exceeds limit ${}",
                    domain, current, limit
                )
            }
            BlockReason::DrawdownExceeded { limit, current } => {
                write!(f, "Drawdown ${} exceeds limit ${}", current, limit)
            }
            BlockReason::MarketNotAllowed { market, agent } => {
                write!(f, "Agent {} not allowed in market {}", agent, market)
            }
            BlockReason::AgentNotActive { agent, status } => {
                write!(f, "Agent {} is {} (not active)", agent, status)
            }
            BlockReason::UnregisteredAgent { agent } => {
                write!(f, "Agent {} is not registered for risk controls", agent)
            }
            BlockReason::OrderExpired => write!(f, "Order has expired"),
            BlockReason::TooManyUnhedgedPositions { limit, current } => {
                write!(f, "Unhedged positions {} exceeds limit {}", current, limit)
            }
        }
    }
}

/// 調整建議
#[derive(Debug, Clone)]
pub struct AdjustmentSuggestion {
    /// 建議的最大數量
    pub max_shares: u64,
    /// 原因
    pub reason: String,
}

/// 平台風控狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlatformRiskState {
    /// 正常
    Normal,
    /// 警戒 (減少新開倉)
    Elevated,
    /// 熔斷 (停止交易)
    Halted,
}

impl Default for PlatformRiskState {
    fn default() -> Self {
        PlatformRiskState::Normal
    }
}

impl PlatformRiskState {
    pub fn can_trade(&self) -> bool {
        !matches!(self, PlatformRiskState::Halted)
    }

    pub fn can_open_new(&self) -> bool {
        matches!(self, PlatformRiskState::Normal)
    }
}

/// Circuit breaker state transitions (for UI/audit)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CircuitBreakerEvent {
    pub timestamp: DateTime<Utc>,
    pub reason: String,
    pub state: PlatformRiskState,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct DrawdownSnapshot {
    pub current_equity: Decimal,
    pub equity_peak: Decimal,
    pub current_drawdown: Decimal,
    pub max_drawdown_observed: Decimal,
}
