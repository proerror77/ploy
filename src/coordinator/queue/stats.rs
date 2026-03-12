use super::OrderQueue;

impl OrderQueue {
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
