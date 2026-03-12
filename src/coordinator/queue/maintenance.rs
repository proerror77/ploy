use chrono::Utc;
use rust_decimal::Decimal;
use tracing::debug;

use super::{OrderIntent, OrderQueue};
use crate::domain::{Domain, Side};

impl OrderQueue {
    /// 清理過期訂單
    pub fn cleanup_expired(&mut self) -> usize {
        self.cleanup_expired_intents().len()
    }

    /// 清理過期訂單並返回被移除的 intents（供上層釋放預留資金）
    pub fn cleanup_expired_intents(&mut self) -> Vec<OrderIntent> {
        let before = self.heap.len();
        let now = Utc::now();
        let mut expired = Vec::new();

        // 需要重建堆，因為 BinaryHeap 不支持條件刪除
        let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();

        for item in items {
            if let Some(expires) = item.intent.expires_at {
                if now > expires {
                    self.expired_count += 1;
                    expired.push(item.intent);
                    continue;
                }
            }
            self.heap.push(item);
        }

        let cleaned = before - self.heap.len();
        if cleaned > 0 {
            debug!("Cleaned {} expired orders from queue", cleaned);
        }
        expired
    }

    /// 移除指定 Agent 的所有訂單
    pub fn remove_agent_orders(&mut self, agent_id: &str) -> usize {
        let before = self.heap.len();
        let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();

        for item in items {
            if item.intent.agent_id != agent_id {
                self.heap.push(item);
            }
        }

        before - self.heap.len()
    }

    /// 移除 queue 中待執行 BUY 訂單（可選限定 domain），並返回被移除的 intents。
    pub fn remove_buy_orders(&mut self, domain: Option<Domain>) -> Vec<OrderIntent> {
        let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();
        let mut removed = Vec::new();

        for item in items {
            let should_remove = item.intent.is_buy
                && domain
                    .map(|target| item.intent.domain == target)
                    .unwrap_or(true);
            if should_remove {
                removed.push(item.intent);
            } else {
                self.heap.push(item);
            }
        }

        removed
    }

    /// Sum buy-intent notionals in queue, excluding specific domains.
    pub fn pending_buy_notional_excluding_domains(&self, excluded: &[Domain]) -> Decimal {
        self.heap
            .iter()
            .filter_map(|item| {
                let intent = &item.intent;
                (intent.is_buy && !excluded.contains(&intent.domain))
                    .then_some(intent.notional_value())
            })
            .sum()
    }

    /// Sum pending SELL shares in queue for one reduce-only bucket.
    pub fn pending_sell_shares_for(
        &self,
        agent_id: &str,
        domain: Domain,
        token_id: &str,
        side: Side,
    ) -> u64 {
        self.heap
            .iter()
            .filter_map(|item| {
                let intent = &item.intent;
                (!intent.is_buy
                    && !intent.is_expired()
                    && intent.agent_id == agent_id
                    && intent.domain == domain
                    && intent.side == side
                    && intent.token_id.eq_ignore_ascii_case(token_id))
                .then_some(intent.shares)
            })
            .fold(0u64, |acc, shares| acc.saturating_add(shares))
    }
}
