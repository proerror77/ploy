//! Core status and risk types for the order plane.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Agent 狀態
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// 初始化中
    Initializing,
    /// 運行中
    Running,
    /// 暫停
    Paused,
    /// 僅監控 (不下單)
    Observing,
    /// 已停止
    Stopped,
    /// 錯誤狀態
    Error,
}

impl AgentStatus {
    pub fn can_trade(&self) -> bool {
        matches!(self, AgentStatus::Running)
    }

    pub fn is_active(&self) -> bool {
        matches!(
            self,
            AgentStatus::Running | AgentStatus::Observing | AgentStatus::Paused
        )
    }
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Initializing => write!(f, "Initializing"),
            AgentStatus::Running => write!(f, "Running"),
            AgentStatus::Paused => write!(f, "Paused"),
            AgentStatus::Observing => write!(f, "Observing"),
            AgentStatus::Stopped => write!(f, "Stopped"),
            AgentStatus::Error => write!(f, "Error"),
        }
    }
}

/// Agent 風險參數
///
/// 由 bootstrap / deployment / governance 註冊到風控層，不再由
/// runtime trait 本身作為 canonical owner。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRiskParams {
    /// 單筆最大下單金額 (USD)
    pub max_order_value: Decimal,
    /// 最大總倉位 (USD)
    pub max_total_exposure: Decimal,
    /// 最大未對沖倉位數量
    pub max_unhedged_positions: u32,
    /// 單日最大虧損 (USD)
    pub max_daily_loss: Decimal,
    /// 是否允許隔夜持倉
    pub allow_overnight: bool,
    /// 允許的市場 slugs (空 = 全部允許)
    pub allowed_markets: Vec<String>,
}

impl Default for AgentRiskParams {
    fn default() -> Self {
        Self {
            max_order_value: Decimal::from(50),
            max_total_exposure: Decimal::from(200),
            max_unhedged_positions: 3,
            max_daily_loss: Decimal::from(100),
            allow_overnight: false,
            allowed_markets: vec![],
        }
    }
}

impl AgentRiskParams {
    pub fn governance_only() -> Self {
        Self {
            max_order_value: Decimal::ZERO,
            max_total_exposure: Decimal::ZERO,
            max_unhedged_positions: 0,
            max_daily_loss: Decimal::ZERO,
            allow_overnight: false,
            allowed_markets: vec![],
        }
    }

    pub fn conservative() -> Self {
        Self {
            max_order_value: Decimal::from(25),
            max_total_exposure: Decimal::from(100),
            max_unhedged_positions: 2,
            max_daily_loss: Decimal::from(50),
            allow_overnight: false,
            allowed_markets: vec![],
        }
    }

    pub fn aggressive() -> Self {
        Self {
            max_order_value: Decimal::from(100),
            max_total_exposure: Decimal::from(500),
            max_unhedged_positions: 5,
            max_daily_loss: Decimal::from(200),
            allow_overnight: true,
            allowed_markets: vec![],
        }
    }

    /// 檢查市場是否被允許
    pub fn is_market_allowed(&self, market_slug: &str) -> bool {
        self.allowed_markets.is_empty() || self.allowed_markets.contains(&market_slug.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_status() {
        assert!(AgentStatus::Running.can_trade());
        assert!(!AgentStatus::Paused.can_trade());
        assert!(AgentStatus::Paused.is_active());
        assert!(!AgentStatus::Stopped.is_active());
    }

    #[test]
    fn test_risk_params() {
        let params = AgentRiskParams::default();
        assert!(params.is_market_allowed("any-market"));

        let mut restricted = params.clone();
        restricted.allowed_markets = vec!["btc-15m".to_string()];
        assert!(restricted.is_market_allowed("btc-15m"));
        assert!(!restricted.is_market_allowed("eth-15m"));
    }

    #[test]
    fn test_governance_only_risk_params() {
        let params = AgentRiskParams::governance_only();

        assert_eq!(params.max_order_value, Decimal::ZERO);
        assert_eq!(params.max_total_exposure, Decimal::ZERO);
        assert_eq!(params.max_unhedged_positions, 0);
        assert_eq!(params.max_daily_loss, Decimal::ZERO);
        assert!(!params.allow_overnight);
        assert!(params.allowed_markets.is_empty());
    }
}
