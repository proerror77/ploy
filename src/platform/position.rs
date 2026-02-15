//! Position Aggregator - 倉位聚合管理
//!
//! 跨 Agent 和領域的統一倉位追蹤。
//! 提供組合級別的暴露、損益和風險視圖。

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};

use super::types::Domain;
use crate::domain::Side;

/// 單一倉位
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    /// 倉位 ID
    pub position_id: String,
    /// 所屬 Agent
    pub agent_id: String,
    /// 領域
    pub domain: Domain,
    /// 市場 slug
    pub market_slug: String,
    /// Token ID
    pub token_id: String,
    /// 方向 (Up/Down)
    pub side: Side,
    /// 數量
    pub shares: u64,
    /// 入場價
    pub entry_price: Decimal,
    /// 當前價格 (用於計算未實現損益)
    pub current_price: Option<Decimal>,
    /// 是否已對沖
    pub is_hedged: bool,
    /// 入場時間
    pub entry_time: DateTime<Utc>,
    /// 更新時間
    pub updated_at: DateTime<Utc>,
    /// 元數據
    pub metadata: HashMap<String, String>,
}

impl Position {
    /// 計算倉位價值
    pub fn notional_value(&self) -> Decimal {
        self.entry_price * Decimal::from(self.shares)
    }

    /// 計算未實現損益
    pub fn unrealized_pnl(&self) -> Decimal {
        match self.current_price {
            Some(current) => (current - self.entry_price) * Decimal::from(self.shares),
            None => Decimal::ZERO,
        }
    }

    /// 更新當前價格
    pub fn update_price(&mut self, price: Decimal) {
        self.current_price = Some(price);
        self.updated_at = Utc::now();
    }

    /// 標記為已對沖
    pub fn mark_hedged(&mut self) {
        self.is_hedged = true;
        self.updated_at = Utc::now();
    }

    /// 持倉時間 (秒)
    pub fn holding_duration_secs(&self) -> i64 {
        (Utc::now() - self.entry_time).num_seconds()
    }
}

/// 聚合後的倉位視圖
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregatedPosition {
    /// 總暴露 (USD)
    pub total_exposure: Decimal,
    /// 未實現損益
    pub unrealized_pnl: Decimal,
    /// 已實現損益 (今日)
    pub realized_pnl: Decimal,
    /// 倉位數量
    pub position_count: usize,
    /// 未對沖倉位數量
    pub unhedged_count: usize,
    /// 按領域分組的暴露
    pub exposure_by_domain: HashMap<Domain, Decimal>,
    /// 按 Agent 分組的暴露
    pub exposure_by_agent: HashMap<String, Decimal>,
    /// 按市場分組的暴露
    pub exposure_by_market: HashMap<String, Decimal>,
}

impl AggregatedPosition {
    /// 最大單一領域暴露
    pub fn max_domain_exposure(&self) -> Decimal {
        self.exposure_by_domain
            .values()
            .cloned()
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// 最大單一 Agent 暴露
    pub fn max_agent_exposure(&self) -> Decimal {
        self.exposure_by_agent
            .values()
            .cloned()
            .max()
            .unwrap_or(Decimal::ZERO)
    }

    /// 最大單一市場暴露
    pub fn max_market_exposure(&self) -> Decimal {
        self.exposure_by_market
            .values()
            .cloned()
            .max()
            .unwrap_or(Decimal::ZERO)
    }
}

/// Agent 級別統計
#[derive(Debug, Clone, Default)]
pub struct AgentPositionStats {
    /// 總暴露
    pub exposure: Decimal,
    /// 未實現損益
    pub unrealized_pnl: Decimal,
    /// 倉位數量
    pub position_count: usize,
    /// 未對沖數量
    pub unhedged_count: usize,
}

/// 倉位聚合器
///
/// 管理所有 Agent 的倉位，提供統一視圖。
pub struct PositionAggregator {
    /// 所有倉位 (position_id -> Position)
    positions: Arc<RwLock<HashMap<String, Position>>>,
    /// 已實現損益 (agent_id -> pnl)
    realized_pnl: Arc<RwLock<HashMap<String, Decimal>>>,
    /// 倉位 ID 計數器
    position_counter: Arc<RwLock<u64>>,
}

impl Default for PositionAggregator {
    fn default() -> Self {
        Self::new()
    }
}

impl PositionAggregator {
    /// 創建新的聚合器
    pub fn new() -> Self {
        Self {
            positions: Arc::new(RwLock::new(HashMap::new())),
            realized_pnl: Arc::new(RwLock::new(HashMap::new())),
            position_counter: Arc::new(RwLock::new(0)),
        }
    }

    // ==================== 倉位操作 ====================

    /// 開倉
    pub async fn open_position(
        &self,
        agent_id: &str,
        domain: Domain,
        market_slug: &str,
        token_id: &str,
        side: Side,
        shares: u64,
        entry_price: Decimal,
    ) -> String {
        let position_id = {
            let mut counter = self.position_counter.write().await;
            *counter += 1;
            format!("pos-{}-{}", agent_id, counter)
        };

        let position = Position {
            position_id: position_id.clone(),
            agent_id: agent_id.to_string(),
            domain,
            market_slug: market_slug.to_string(),
            token_id: token_id.to_string(),
            side,
            shares,
            entry_price,
            current_price: Some(entry_price),
            is_hedged: false,
            entry_time: Utc::now(),
            updated_at: Utc::now(),
            metadata: HashMap::new(),
        };

        info!(
            "Opened position {} for agent {} in {}/{}: {} shares @ {}",
            position_id, agent_id, domain, market_slug, shares, entry_price
        );

        self.positions
            .write()
            .await
            .insert(position_id.clone(), position);
        position_id
    }

    /// 平倉
    pub async fn close_position(&self, position_id: &str, exit_price: Decimal) -> Option<Decimal> {
        let mut positions = self.positions.write().await;

        if let Some(position) = positions.remove(position_id) {
            let pnl = (exit_price - position.entry_price) * Decimal::from(position.shares);

            // 記錄已實現損益
            let mut realized = self.realized_pnl.write().await;
            *realized
                .entry(position.agent_id.clone())
                .or_insert(Decimal::ZERO) += pnl;

            info!(
                "Closed position {} for agent {}: {} shares @ {} (PnL: {})",
                position_id, position.agent_id, position.shares, exit_price, pnl
            );

            Some(pnl)
        } else {
            None
        }
    }

    /// 更新倉位價格
    pub async fn update_price(&self, position_id: &str, price: Decimal) {
        if let Some(position) = self.positions.write().await.get_mut(position_id) {
            position.update_price(price);
        }
    }

    /// 批量更新市場價格
    pub async fn update_market_prices(&self, market_slug: &str, prices: &HashMap<String, Decimal>) {
        let mut positions = self.positions.write().await;
        for position in positions.values_mut() {
            if position.market_slug == market_slug {
                if let Some(&price) = prices.get(&position.token_id) {
                    position.update_price(price);
                }
            }
        }
    }

    /// 標記倉位為已對沖
    pub async fn mark_hedged(&self, position_id: &str) {
        if let Some(position) = self.positions.write().await.get_mut(position_id) {
            position.mark_hedged();
            debug!("Position {} marked as hedged", position_id);
        }
    }

    // ==================== 查詢方法 ====================

    /// 獲取單個倉位
    pub async fn get_position(&self, position_id: &str) -> Option<Position> {
        self.positions.read().await.get(position_id).cloned()
    }

    /// 獲取 Agent 所有倉位
    pub async fn get_agent_positions(&self, agent_id: &str) -> Vec<Position> {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| p.agent_id == agent_id)
            .cloned()
            .collect()
    }

    /// 獲取市場所有倉位
    pub async fn get_market_positions(&self, market_slug: &str) -> Vec<Position> {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| p.market_slug == market_slug)
            .cloned()
            .collect()
    }

    /// 獲取領域所有倉位
    pub async fn get_domain_positions(&self, domain: Domain) -> Vec<Position> {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| p.domain == domain)
            .cloned()
            .collect()
    }

    /// 獲取所有倉位
    pub async fn all_positions(&self) -> Vec<Position> {
        self.positions.read().await.values().cloned().collect()
    }

    // ==================== 聚合統計 ====================

    /// 獲取聚合視圖
    pub async fn aggregate(&self) -> AggregatedPosition {
        let positions = self.positions.read().await;
        let realized = self.realized_pnl.read().await;

        let mut result = AggregatedPosition::default();

        for position in positions.values() {
            let exposure = position.notional_value();
            let pnl = position.unrealized_pnl();

            result.total_exposure += exposure;
            result.unrealized_pnl += pnl;
            result.position_count += 1;

            if !position.is_hedged {
                result.unhedged_count += 1;
            }

            *result
                .exposure_by_domain
                .entry(position.domain)
                .or_insert(Decimal::ZERO) += exposure;
            *result
                .exposure_by_agent
                .entry(position.agent_id.clone())
                .or_insert(Decimal::ZERO) += exposure;
            *result
                .exposure_by_market
                .entry(position.market_slug.clone())
                .or_insert(Decimal::ZERO) += exposure;
        }

        result.realized_pnl = realized.values().sum();

        result
    }

    /// 獲取 Agent 統計
    pub async fn agent_stats(&self, agent_id: &str) -> AgentPositionStats {
        let positions = self.positions.read().await;
        let _realized = self.realized_pnl.read().await;

        let mut stats = AgentPositionStats::default();

        for position in positions.values() {
            if position.agent_id == agent_id {
                stats.exposure += position.notional_value();
                stats.unrealized_pnl += position.unrealized_pnl();
                stats.position_count += 1;
                if !position.is_hedged {
                    stats.unhedged_count += 1;
                }
            }
        }

        stats
    }

    /// 獲取總暴露
    pub async fn total_exposure(&self) -> Decimal {
        self.positions
            .read()
            .await
            .values()
            .map(|p| p.notional_value())
            .sum()
    }

    /// 獲取 Agent 暴露
    pub async fn agent_exposure(&self, agent_id: &str) -> Decimal {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| p.agent_id == agent_id)
            .map(|p| p.notional_value())
            .sum()
    }

    /// 獲取總未實現損益
    pub async fn total_unrealized_pnl(&self) -> Decimal {
        self.positions
            .read()
            .await
            .values()
            .map(|p| p.unrealized_pnl())
            .sum()
    }

    /// 獲取總已實現損益
    pub async fn total_realized_pnl(&self) -> Decimal {
        self.realized_pnl.read().await.values().sum()
    }

    /// 獲取 Agent 已實現損益
    pub async fn agent_realized_pnl(&self, agent_id: &str) -> Decimal {
        self.realized_pnl
            .read()
            .await
            .get(agent_id)
            .cloned()
            .unwrap_or(Decimal::ZERO)
    }

    /// 倉位總數
    pub async fn position_count(&self) -> usize {
        self.positions.read().await.len()
    }

    /// 未對沖倉位數
    pub async fn unhedged_count(&self) -> usize {
        self.positions
            .read()
            .await
            .values()
            .filter(|p| !p.is_hedged)
            .count()
    }

    // ==================== 清理 ====================

    /// 清理所有數據
    pub async fn clear(&self) {
        self.positions.write().await.clear();
        self.realized_pnl.write().await.clear();
        *self.position_counter.write().await = 0;
    }

    /// 清理 Agent 數據
    pub async fn clear_agent(&self, agent_id: &str) {
        self.positions
            .write()
            .await
            .retain(|_, p| p.agent_id != agent_id);
        self.realized_pnl.write().await.remove(agent_id);
    }

    /// 移除過期倉位 (根據 metadata 中的 expires_at)
    pub async fn cleanup_expired(&self) -> usize {
        let now = Utc::now();
        let mut positions = self.positions.write().await;
        let before = positions.len();

        positions.retain(|_, p| {
            if let Some(expires_str) = p.metadata.get("expires_at") {
                if let Ok(expires) = expires_str.parse::<DateTime<Utc>>() {
                    return now < expires;
                }
            }
            true
        });

        before - positions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_open_close_position() {
        let agg = PositionAggregator::new();

        // 開倉
        let pos_id = agg
            .open_position(
                "agent1",
                Domain::Crypto,
                "btc-15m",
                "token-yes",
                Side::Up,
                100,
                Decimal::from_str_exact("0.50").unwrap(),
            )
            .await;

        assert_eq!(agg.position_count().await, 1);
        assert_eq!(agg.total_exposure().await, Decimal::from(50));

        // 平倉
        let pnl = agg
            .close_position(&pos_id, Decimal::from_str_exact("0.55").unwrap())
            .await;
        assert!(pnl.is_some());
        assert_eq!(pnl.unwrap(), Decimal::from(5)); // (0.55 - 0.50) * 100 = 5

        assert_eq!(agg.position_count().await, 0);
        assert_eq!(agg.total_realized_pnl().await, Decimal::from(5));
    }

    #[tokio::test]
    async fn test_aggregate() {
        let agg = PositionAggregator::new();

        // 開多個倉位
        agg.open_position(
            "agent1",
            Domain::Crypto,
            "btc-15m",
            "t1",
            Side::Up,
            100,
            Decimal::from_str_exact("0.50").unwrap(),
        )
        .await;
        agg.open_position(
            "agent1",
            Domain::Crypto,
            "eth-15m",
            "t2",
            Side::Down,
            50,
            Decimal::from_str_exact("0.40").unwrap(),
        )
        .await;
        agg.open_position(
            "agent2",
            Domain::Sports,
            "nba-123",
            "t3",
            Side::Up,
            200,
            Decimal::from_str_exact("0.60").unwrap(),
        )
        .await;

        let aggregate = agg.aggregate().await;

        assert_eq!(aggregate.position_count, 3);
        assert_eq!(aggregate.unhedged_count, 3);
        // 50 + 20 + 120 = 190
        assert_eq!(aggregate.total_exposure, Decimal::from(190));

        // 按領域
        assert_eq!(
            aggregate.exposure_by_domain.get(&Domain::Crypto),
            Some(&Decimal::from(70))
        );
        assert_eq!(
            aggregate.exposure_by_domain.get(&Domain::Sports),
            Some(&Decimal::from(120))
        );

        // 按 Agent
        assert_eq!(
            aggregate.exposure_by_agent.get("agent1"),
            Some(&Decimal::from(70))
        );
        assert_eq!(
            aggregate.exposure_by_agent.get("agent2"),
            Some(&Decimal::from(120))
        );
    }

    #[tokio::test]
    async fn test_update_price() {
        let agg = PositionAggregator::new();

        let pos_id = agg
            .open_position(
                "agent1",
                Domain::Crypto,
                "btc-15m",
                "token-yes",
                Side::Up,
                100,
                Decimal::from_str_exact("0.50").unwrap(),
            )
            .await;

        // 初始未實現損益為 0
        assert_eq!(agg.total_unrealized_pnl().await, Decimal::ZERO);

        // 更新價格
        agg.update_price(&pos_id, Decimal::from_str_exact("0.55").unwrap())
            .await;

        // 未實現損益 = (0.55 - 0.50) * 100 = 5
        assert_eq!(agg.total_unrealized_pnl().await, Decimal::from(5));
    }

    #[tokio::test]
    async fn test_agent_stats() {
        let agg = PositionAggregator::new();

        agg.open_position(
            "agent1",
            Domain::Crypto,
            "btc-15m",
            "t1",
            Side::Up,
            100,
            Decimal::from_str_exact("0.50").unwrap(),
        )
        .await;
        agg.open_position(
            "agent1",
            Domain::Crypto,
            "eth-15m",
            "t2",
            Side::Down,
            50,
            Decimal::from_str_exact("0.40").unwrap(),
        )
        .await;
        agg.open_position(
            "agent2",
            Domain::Sports,
            "nba-123",
            "t3",
            Side::Up,
            200,
            Decimal::from_str_exact("0.60").unwrap(),
        )
        .await;

        let stats = agg.agent_stats("agent1").await;
        assert_eq!(stats.exposure, Decimal::from(70));
        assert_eq!(stats.position_count, 2);
        assert_eq!(stats.unhedged_count, 2);

        let stats2 = agg.agent_stats("agent2").await;
        assert_eq!(stats2.exposure, Decimal::from(120));
        assert_eq!(stats2.position_count, 1);
    }
}
