//! Order Queue - 優先級訂單隊列

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use tracing::{debug, warn};

use crate::coordinator::OrderIntent;

#[path = "queue/maintenance.rs"]
mod maintenance;
#[path = "queue/stats.rs"]
mod stats;
pub use stats::QueueStats;
#[cfg(test)]
#[path = "queue/tests.rs"]
mod tests;

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
        match other.intent.priority.cmp(&self.intent.priority) {
            Ordering::Equal => other.sequence.cmp(&self.sequence),
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
    pub fn enqueue(&mut self, intent: OrderIntent) -> Result<(), String> {
        self.enqueue_with_eviction(intent).map(|_| ())
    }

    /// Enqueue and return evicted low-priority intent when queue is full.
    pub fn enqueue_with_eviction(
        &mut self,
        intent: OrderIntent,
    ) -> Result<Option<OrderIntent>, String> {
        let mut evicted: Option<OrderIntent> = None;
        if self.heap.len() >= self.max_size {
            if let Some(lowest_priority) = self.heap.iter().map(|item| item.intent.priority).max() {
                if intent.priority < lowest_priority {
                    evicted = self.pop_lowest_priority().map(|v| v.intent);
                    warn!(
                        "Queue full, dropped lowest priority order to make room for {:?}",
                        intent.priority
                    );
                } else {
                    return Err("Queue is full and new order has lower priority".to_string());
                }
            }
        }

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

        Ok(evicted)
    }

    /// Pop one lowest-priority item from queue.
    /// When priorities tie, evict the newest item in that lowest bucket.
    fn pop_lowest_priority(&mut self) -> Option<PrioritizedIntent> {
        let mut items: Vec<PrioritizedIntent> = std::mem::take(&mut self.heap).into_vec();
        if items.is_empty() {
            return None;
        }

        let mut lowest_idx = 0usize;
        for idx in 1..items.len() {
            let current = &items[idx];
            let lowest = &items[lowest_idx];
            if current.intent.priority > lowest.intent.priority
                || (current.intent.priority == lowest.intent.priority
                    && current.sequence > lowest.sequence)
            {
                lowest_idx = idx;
            }
        }

        let dropped = items.swap_remove(lowest_idx);
        self.heap = BinaryHeap::from(items);
        Some(dropped)
    }

    /// 取出下一個要執行的訂單
    pub fn dequeue(&mut self) -> Option<OrderIntent> {
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
}
