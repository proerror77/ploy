//! Buffered signal recorder.
//!
//! Accumulates signal records in memory and flushes them in batches
//! via a user-supplied async callback. Suitable for writing to
//! PostgreSQL, CSV, or any other persistence backend.

use std::future::Future;
use std::pin::Pin;

use async_trait::async_trait;
use tracing::info;

use crate::traits::{Recorder, SignalRecord};

/// Type alias for the async flush callback.
pub type FlushFn = Box<
    dyn Fn(Vec<SignalRecord>) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Recorder that buffers signals and flushes in batches.
pub struct BufferedRecorder {
    buffer: Vec<SignalRecord>,
    batch_size: usize,
    flush_fn: Option<FlushFn>,
}

impl BufferedRecorder {
    /// Create a recorder that flushes every `batch_size` signals.
    ///
    /// If `flush_fn` is `None`, signals are buffered but discarded on flush
    /// (useful for dry-run without persistence).
    pub fn new(batch_size: usize, flush_fn: Option<FlushFn>) -> Self {
        Self {
            buffer: Vec::with_capacity(batch_size),
            batch_size,
            flush_fn,
        }
    }

    /// Create a recorder that just counts signals (no persistence).
    pub fn counting() -> Self {
        Self::new(1000, None)
    }

    /// Number of signals recorded (including flushed).
    pub fn total_buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Drain the buffer (for testing or manual access).
    pub fn drain(&mut self) -> Vec<SignalRecord> {
        std::mem::take(&mut self.buffer)
    }

    async fn do_flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let batch = std::mem::take(&mut self.buffer);
        let count = batch.len();
        if let Some(ref flush_fn) = self.flush_fn {
            flush_fn(batch).await;
        }
        info!(count, "Flushed signal records");
    }
}

#[async_trait]
impl Recorder for BufferedRecorder {
    async fn record_signal(&mut self, signal: &SignalRecord) {
        self.buffer.push(signal.clone());
        if self.buffer.len() >= self.batch_size {
            self.do_flush().await;
        }
    }

    async fn flush(&mut self) {
        self.do_flush().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rust_decimal_macros::dec;
    use std::sync::{Arc, Mutex};

    fn sample_signal() -> SignalRecord {
        SignalRecord {
            strategy: "test".into(),
            symbol: "BTCUSDT".into(),
            direction: "UP".into(),
            p_hat: 0.75,
            edge: 0.10,
            entry_price: dec!(0.30),
            decision: "enter".into(),
            ts: Utc::now(),
        }
    }

    #[tokio::test]
    async fn buffers_and_flushes() {
        let flushed = Arc::new(Mutex::new(Vec::new()));
        let flushed_clone = flushed.clone();

        let flush_fn: FlushFn = Box::new(move |batch| {
            let flushed = flushed_clone.clone();
            Box::pin(async move {
                flushed.lock().unwrap().extend(batch);
            })
        });

        let mut recorder = BufferedRecorder::new(2, Some(flush_fn));

        recorder.record_signal(&sample_signal()).await;
        assert_eq!(flushed.lock().unwrap().len(), 0); // not yet

        recorder.record_signal(&sample_signal()).await;
        assert_eq!(flushed.lock().unwrap().len(), 2); // batch triggered

        recorder.record_signal(&sample_signal()).await;
        recorder.flush().await;
        assert_eq!(flushed.lock().unwrap().len(), 3); // manual flush
    }

    #[tokio::test]
    async fn counting_mode_no_panic() {
        let mut recorder = BufferedRecorder::counting();
        recorder.record_signal(&sample_signal()).await;
        recorder.record_signal(&sample_signal()).await;
        recorder.flush().await;
        // No panic, signals discarded
    }
}
