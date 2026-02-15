//! Core traits for the Order Platform

use async_trait::async_trait;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use super::types::{Domain, DomainEvent, ExecutionReport, OrderIntent};
use crate::error::Result;

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

/// 領域策略 Agent trait
///
/// 所有策略 Agent 必須實作這個 trait 才能接入下單平台。
/// 每個 Agent 負責：
/// - 接收並處理領域事件
/// - 產生下單意圖
/// - 處理執行結果回調
#[async_trait]
pub trait DomainAgent: Send + Sync {
    /// Agent 唯一 ID
    fn id(&self) -> &str;

    /// Agent 名稱
    fn name(&self) -> &str;

    /// 所屬領域
    fn domain(&self) -> Domain;

    /// 當前狀態
    fn status(&self) -> AgentStatus;

    /// 風險參數
    fn risk_params(&self) -> &AgentRiskParams;

    /// 處理領域事件
    ///
    /// 當有新的市場數據或事件時調用。
    /// Agent 分析事件並決定是否下單。
    ///
    /// # Arguments
    /// * `event` - 領域事件
    ///
    /// # Returns
    /// 下單意圖列表 (可為空)
    async fn on_event(&mut self, event: DomainEvent) -> Result<Vec<OrderIntent>>;

    /// 處理執行報告
    ///
    /// 當訂單執行完成 (成功或失敗) 時調用。
    /// Agent 更新內部狀態。
    async fn on_execution(&mut self, report: ExecutionReport);

    /// 啟動 Agent
    async fn start(&mut self) -> Result<()>;

    /// 停止 Agent
    async fn stop(&mut self) -> Result<()>;

    /// 暫停交易 (保持監控)
    fn pause(&mut self);

    /// 恢復交易
    fn resume(&mut self);

    /// 獲取當前倉位數量
    fn position_count(&self) -> usize;

    /// 獲取當前總暴露
    fn total_exposure(&self) -> Decimal;

    /// 獲取今日 PnL
    fn daily_pnl(&self) -> Decimal;

    /// 健康檢查
    fn health_check(&self) -> AgentHealthStatus {
        AgentHealthStatus {
            agent_id: self.id().to_string(),
            status: self.status(),
            position_count: self.position_count(),
            total_exposure: self.total_exposure(),
            daily_pnl: self.daily_pnl(),
            is_healthy: self.status().is_active(),
        }
    }
}

/// Agent 健康狀態
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentHealthStatus {
    pub agent_id: String,
    pub status: AgentStatus,
    pub position_count: usize,
    pub total_exposure: Decimal,
    pub daily_pnl: Decimal,
    pub is_healthy: bool,
}

/// 簡化的 Agent 實作輔助 trait
///
/// 提供一些默認實作，減少樣板代碼
pub trait SimpleAgent: DomainAgent {
    /// 更新狀態
    fn set_status(&mut self, status: AgentStatus);
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
}
