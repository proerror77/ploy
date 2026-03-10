use super::*;

impl PositionAggregator {
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

    /// Reduce shares from an existing position (supports partial close).
    /// Returns realized PnL for the reduced shares.
    pub async fn reduce_position(
        &self,
        position_id: &str,
        reduce_shares: u64,
        exit_price: Decimal,
    ) -> Option<Decimal> {
        if reduce_shares == 0 {
            return Some(Decimal::ZERO);
        }

        let mut positions = self.positions.write().await;
        let mut remove_entry = false;

        let (agent_id, reduced, pnl) = {
            let position = positions.get_mut(position_id)?;
            let reduced = reduce_shares.min(position.shares);
            let pnl = (exit_price - position.entry_price) * Decimal::from(reduced);
            position.shares -= reduced;
            if position.shares == 0 {
                remove_entry = true;
            } else {
                position.updated_at = Utc::now();
            }
            (position.agent_id.clone(), reduced, pnl)
        };

        if reduced == 0 {
            return Some(Decimal::ZERO);
        }

        if remove_entry {
            positions.remove(position_id);
        }

        drop(positions);

        let mut realized = self.realized_pnl.write().await;
        *realized.entry(agent_id).or_insert(Decimal::ZERO) += pnl;

        Some(pnl)
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
