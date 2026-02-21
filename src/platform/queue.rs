//! Order Queue - 優先級訂單隊列

use chrono::Utc;
use rust_decimal::Decimal;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use tracing::{debug, warn};

use super::types::{Domain, OrderIntent};

/// 包裝 OrderIntent 以支持優先級排序
#[derive(Debug)]
struct PrioritizedIntent {
    intent: OrderIntent,
    sequence: u64, // 用於相同優先級時的 FIFO 排序
}

impl PartialEq for PrioritizedIntent {
    fn eq(&self, other: &Self) -> bool {
        self.intent.priority == other.intent.priority && self.sequence == other.sequence
    }
}

impl Eq for PrioritizedIntent {}

impl PartialOrd for PrioritizedIntent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedIntent {
    fn cmp(&self, other: &Self) -> Ordering {
        // 優先級數字越小越優先 (Critical=0 > High=1 > Normal=2 > Low=3)
        match other.intent.priority.cmp(&self.intent.priority) {
            Ordering::Equal => {
                // 相同優先級，先進先出 (sequence 越小越早)
                other.sequence.cmp(&self.sequence)
            }
            ord => ord,
        }
    }
}

/// 訂單隊列 - 基於優先級的訂單排隊系統
pub struct OrderQueue {
    /// 優先級堆
    heap: BinaryHeap<PrioritizedIntent>,
    /// 序列號計數器
    sequence_counter: u64,
    /// 最大隊列長度
    max_size: usize,
    /// 統計：已入隊數量
    enqueued_count: u64,
    /// 統計：已出隊數量
    dequeued_count: u64,
    /// 統計：已過期數量
    expired_count: u64,
}

impl OrderQueue {
    /// 創建新隊列
    pub fn new(max_size: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            sequence_counter: 0,
            max_size,
            enqueued_count: 0,
            dequeued_count: 0,
            expired_count: 0,
        }
    }

    /// 將訂單意圖加入隊列
    ///
    /// # Returns
    /// - `Ok(())` 成功入隊
    /// - `Err(reason)` 隊列已滿或訂單無效
    pub fn enqueue(&mut self, intent: OrderIntent) -> Result<(), String> {
        // 檢查隊列是否已滿
        if self.heap.len() >= self.max_size {
            // 如果新訂單優先級更高，嘗試移除最低優先級的
            if let Some(lowest) = self.heap.peek() {
                if intent.priority < lowest.intent.priority {
                    // 新訂單優先級更高，移除最低的
                    self.heap.pop();
                    warn!(
                        "Queue full, dropped lowest priority order to make room for {:?}",
                        intent.priority
                    );
                } else {
                    return Err("Queue is full and new order has lower priority".to_string());
                }
            }
        }

        // 檢查是否已過期
        if intent.is_expired() {
            return Err("Order intent has already expired".to_string());
        }

        let sequence = self.sequence_counter;
        self.sequence_counter += 1;

        debug!(
            "Enqueuing order intent {} from agent {} with priority {:?}",
            intent.intent_id, intent.agent_id, intent.priority
        );

        self.heap.push(PrioritizedIntent { intent, sequence });
        self.enqueued_count += 1;

        Ok(())
    }

    /// 取出下一個要執行的訂單
    pub fn dequeue(&mut self) -> Option<OrderIntent> {
        // 跳過已過期的訂單
        while let Some(prioritized) = self.heap.pop() {
            if prioritized.intent.is_expired() {
                self.expired_count += 1;
                debug!(
                    "Skipping expired order intent {}",
                    prioritized.intent.intent_id
                );
                continue;
            }

            self.dequeued_count += 1;
            return Some(prioritized.intent);
        }

        None
    }

    /// 批量取出訂單 (最多 n 個)
    pub fn dequeue_batch(&mut self, n: usize) -> Vec<OrderIntent> {
        let mut batch = Vec::with_capacity(n);
        for _ in 0..n {
            if let Some(intent) = self.dequeue() {
                batch.push(intent);
            } else {
                break;
            }
        }
        batch
    }

    /// 查看隊列頭部 (不移除)
    pub fn peek(&self) -> Option<&OrderIntent> {
        self.heap.peek().map(|p| &p.intent)
    }

    /// 隊列長度
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    /// 隊列是否為空
    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    /// 清理過期訂單
    pub fn cleanup_expired(&mut self) -> usize {
        let before = self.heap.len();
        let now = Utc::now();

        // 需要重建堆，因為 BinaryHeap 不支持條件刪除
        let items: Vec<_> = std::mem::take(&mut self.heap).into_vec();

        for item in items {
            if let Some(expires) = item.intent.expires_at {
                if now > expires {
                    self.expired_count += 1;
                    continue;
                }
            }
            self.heap.push(item);
        }

        let cleaned = before - self.heap.len();
        if cleaned > 0 {
            debug!("Cleaned {} expired orders from queue", cleaned);
        }
        cleaned
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

    /// 獲取隊列統計
    pub fn stats(&self) -> QueueStats {
        let mut priority_counts = [0usize; 4];
        for item in self.heap.iter() {
            let idx = item.intent.priority as usize;
            if idx < 4 {
                priority_counts[idx] += 1;
            }
        }

        QueueStats {
            current_size: self.heap.len(),
            max_size: self.max_size,
            enqueued_total: self.enqueued_count,
            dequeued_total: self.dequeued_count,
            expired_total: self.expired_count,
            critical_count: priority_counts[0],
            high_count: priority_counts[1],
            normal_count: priority_counts[2],
            low_count: priority_counts[3],
        }
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
}

/// 隊列統計
#[derive(Debug, Clone)]
pub struct QueueStats {
    pub current_size: usize,
    pub max_size: usize,
    pub enqueued_total: u64,
    pub dequeued_total: u64,
    pub expired_total: u64,
    pub critical_count: usize,
    pub high_count: usize,
    pub normal_count: usize,
    pub low_count: usize,
}

impl std::fmt::Display for QueueStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Queue[{}/{}, enq={}, deq={}, exp={}, C={}/H={}/N={}/L={}]",
            self.current_size,
            self.max_size,
            self.enqueued_total,
            self.dequeued_total,
            self.expired_total,
            self.critical_count,
            self.high_count,
            self.normal_count,
            self.low_count
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::types::{Domain, OrderPriority};
    use super::*;
    use crate::domain::Side;
    use rust_decimal::Decimal;

    fn make_intent(agent: &str, priority: OrderPriority) -> OrderIntent {
        OrderIntent::new(
            agent,
            Domain::Crypto,
            "test-market",
            "token-123",
            Side::Up,
            true,
            100,
            Decimal::from_str_exact("0.50").unwrap(),
        )
        .with_priority(priority)
    }

    #[test]
    fn test_priority_ordering() {
        let mut queue = OrderQueue::new(100);

        // 入隊順序：Normal, Low, Critical, High
        queue
            .enqueue(make_intent("a1", OrderPriority::Normal))
            .unwrap();
        queue
            .enqueue(make_intent("a2", OrderPriority::Low))
            .unwrap();
        queue
            .enqueue(make_intent("a3", OrderPriority::Critical))
            .unwrap();
        queue
            .enqueue(make_intent("a4", OrderPriority::High))
            .unwrap();

        // 出隊順序應該是：Critical, High, Normal, Low
        assert_eq!(queue.dequeue().unwrap().agent_id, "a3");
        assert_eq!(queue.dequeue().unwrap().agent_id, "a4");
        assert_eq!(queue.dequeue().unwrap().agent_id, "a1");
        assert_eq!(queue.dequeue().unwrap().agent_id, "a2");
    }

    #[test]
    fn test_fifo_same_priority() {
        let mut queue = OrderQueue::new(100);

        queue
            .enqueue(make_intent("first", OrderPriority::Normal))
            .unwrap();
        queue
            .enqueue(make_intent("second", OrderPriority::Normal))
            .unwrap();
        queue
            .enqueue(make_intent("third", OrderPriority::Normal))
            .unwrap();

        assert_eq!(queue.dequeue().unwrap().agent_id, "first");
        assert_eq!(queue.dequeue().unwrap().agent_id, "second");
        assert_eq!(queue.dequeue().unwrap().agent_id, "third");
    }

    #[test]
    fn test_queue_full() {
        let mut queue = OrderQueue::new(2);

        queue
            .enqueue(make_intent("a1", OrderPriority::Normal))
            .unwrap();
        queue
            .enqueue(make_intent("a2", OrderPriority::Normal))
            .unwrap();

        // 滿了，低優先級無法入隊
        let result = queue.enqueue(make_intent("a3", OrderPriority::Low));
        assert!(result.is_err());

        // 高優先級可以入隊 (踢掉最低的)
        queue
            .enqueue(make_intent("a4", OrderPriority::Critical))
            .unwrap();
        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn test_stats() {
        let mut queue = OrderQueue::new(100);

        queue
            .enqueue(make_intent("a1", OrderPriority::Critical))
            .unwrap();
        queue
            .enqueue(make_intent("a2", OrderPriority::High))
            .unwrap();
        queue
            .enqueue(make_intent("a3", OrderPriority::Normal))
            .unwrap();
        queue
            .enqueue(make_intent("a4", OrderPriority::Low))
            .unwrap();

        let stats = queue.stats();
        assert_eq!(stats.current_size, 4);
        assert_eq!(stats.critical_count, 1);
        assert_eq!(stats.high_count, 1);
        assert_eq!(stats.normal_count, 1);
        assert_eq!(stats.low_count, 1);
    }

    #[test]
    fn test_pending_buy_notional_excluding_domains() {
        let mut queue = OrderQueue::new(100);

        let crypto_buy = OrderIntent::new(
            "a1",
            Domain::Crypto,
            "btc-up",
            "token-btc",
            Side::Up,
            true,
            10,
            Decimal::from_str_exact("0.50").unwrap(),
        ); // 5
        let politics_buy = OrderIntent::new(
            "a2",
            Domain::Politics,
            "election-yes",
            "token-pol",
            Side::Up,
            true,
            20,
            Decimal::from_str_exact("0.50").unwrap(),
        ); // 10
        let mut politics_sell = OrderIntent::new(
            "a3",
            Domain::Politics,
            "election-yes",
            "token-pol",
            Side::Up,
            false,
            30,
            Decimal::from_str_exact("0.50").unwrap(),
        ); // ignored
        politics_sell.priority = OrderPriority::Low;

        queue.enqueue(crypto_buy).unwrap();
        queue.enqueue(politics_buy).unwrap();
        queue.enqueue(politics_sell).unwrap();

        let notional =
            queue.pending_buy_notional_excluding_domains(&[Domain::Crypto, Domain::Sports]);
        assert_eq!(notional, Decimal::from_str_exact("10.00").unwrap());
    }
}
