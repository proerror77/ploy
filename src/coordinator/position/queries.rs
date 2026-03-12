use super::{AgentPositionStats, AggregatedPosition, Position, PositionAggregator};
use crate::domain::{Domain, Side};
use rust_decimal::Decimal;

impl PositionAggregator {
    // ==================== 查詢方法 ====================

    /// 獲取單個倉位
    pub async fn get_position(&self, position_id: &str) -> Option<Position> {
        self.positions.read().await.get(position_id).cloned()
    }

    /// 獲取 Agent 所有倉位
    pub async fn get_agent_positions(&self, agent_id: &str) -> Vec<Position> {
        let positions = self.positions.read().await;
        let positions_by_agent = self.positions_by_agent.read().await;

        positions_by_agent
            .get(agent_id)
            .into_iter()
            .flat_map(|position_ids| position_ids.iter())
            .filter_map(|position_id| positions.get(position_id).cloned())
            .collect()
    }

    /// Agent 在特定 token/side 的可用持倉股數（reduce-only SELL 檢查使用）
    pub async fn agent_open_shares_for_token_side(
        &self,
        agent_id: &str,
        domain: Domain,
        token_id: &str,
        side: Side,
    ) -> u64 {
        let positions = self.positions.read().await;
        let positions_by_agent = self.positions_by_agent.read().await;

        positions_by_agent
            .get(agent_id)
            .into_iter()
            .flat_map(|position_ids| position_ids.iter())
            .filter_map(|position_id| positions.get(position_id))
            .filter(|position| {
                position.domain == domain
                    && position.side == side
                    && position.token_id.eq_ignore_ascii_case(token_id)
            })
            .map(|position| position.shares)
            .sum()
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
        let positions_by_agent = self.positions_by_agent.read().await;

        let mut stats = AgentPositionStats::default();

        if let Some(position_ids) = positions_by_agent.get(agent_id) {
            for position_id in position_ids {
                if let Some(position) = positions.get(position_id) {
                    stats.exposure += position.notional_value();
                    stats.unrealized_pnl += position.unrealized_pnl();
                    stats.position_count += 1;
                    if !position.is_hedged {
                        stats.unhedged_count += 1;
                    }
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
}
