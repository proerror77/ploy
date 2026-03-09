use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;

use super::super::types::Domain;

#[derive(Debug, Clone, Default)]
pub(super) struct DrawdownStats {
    pub(super) current_equity: Decimal,
    pub(super) equity_peak: Decimal,
    pub(super) current_drawdown: Decimal,
    pub(super) max_drawdown_observed: Decimal,
}

/// Agent 風控統計
#[derive(Debug, Clone, Default)]
pub(super) struct AgentRiskStats {
    /// 當前暴露
    pub(super) exposure: Decimal,
    /// 未實現損益
    pub(super) unrealized_pnl: Decimal,
    /// 今日已實現損益
    pub(super) realized_pnl: Decimal,
    /// 持倉數量
    pub(super) position_count: usize,
    /// 未對沖倉位數量
    pub(super) unhedged_count: u32,
    /// 連續失敗
    pub(super) consecutive_failures: u32,
    /// 最後更新
    pub(super) last_update: Option<DateTime<Utc>>,
}

/// 每日統計
#[derive(Debug, Clone, Default)]
pub(super) struct DailyStats {
    pub(super) date: Option<NaiveDate>,
    pub(super) total_pnl: Decimal,
    pub(super) domain_pnl: HashMap<Domain, Decimal>,
    pub(super) order_count: u32,
    pub(super) success_count: u32,
    pub(super) failure_count: u32,
}
