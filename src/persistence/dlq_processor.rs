//! Dead Letter Queue Processor
//!
//! Processes failed operations from the dead letter queue with exponential backoff.
//! Supports custom handlers for different operation types.

use crate::adapters::{DLQEntry, TransactionManager};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Configuration for DLQ processor
#[derive(Debug, Clone)]
pub struct DLQProcessorConfig {
    /// Interval between processing cycles (default: 60s)
    pub process_interval_secs: u64,
    /// Maximum entries to process per cycle (default: 10)
    pub batch_size: i64,
    /// Base delay for exponential backoff (default: 1s)
    pub base_backoff_secs: u64,
    /// Maximum backoff delay (default: 3600s / 1 hour)
    pub max_backoff_secs: u64,
}

impl Default for DLQProcessorConfig {
    fn default() -> Self {
        Self {
            process_interval_secs: 60,
            batch_size: 10,
            base_backoff_secs: 1,
            max_backoff_secs: 3600,
        }
    }
}

impl DLQProcessorConfig {
    fn backoff_duration(&self, retry_count: u32) -> Duration {
        let delay = self
            .base_backoff_secs
            .saturating_mul(2u64.saturating_pow(retry_count));
        let capped = delay.min(self.max_backoff_secs);
        Duration::from_secs(capped)
    }
}

/// Result of processing a DLQ entry
#[derive(Debug)]
pub enum DLQResult {
    /// Entry processed successfully, should be marked resolved
    Success,
    /// Entry failed, should be retried
    Retry { error: String },
    /// Entry is permanently failed, no more retries
    PermanentFailure { error: String },
    /// Entry should be skipped for now (e.g., dependency not ready)
    Skip,
}

/// Handler trait for DLQ operations
#[async_trait::async_trait]
pub trait DLQHandler: Send + Sync {
    /// Process a DLQ entry
    async fn process(&self, entry: &DLQEntry) -> DLQResult;

    /// Get operation types this handler supports
    fn supported_types(&self) -> Vec<&'static str>;
}

/// Default handler that just logs
pub struct LoggingHandler;

#[async_trait::async_trait]
impl DLQHandler for LoggingHandler {
    async fn process(&self, entry: &DLQEntry) -> DLQResult {
        warn!(
            "DLQ entry {} (type: {}): {}",
            entry.operation_type, entry.error_message, entry.payload
        );
        DLQResult::Skip
    }

    fn supported_types(&self) -> Vec<&'static str> {
        vec!["*"] // Wildcard for all types
    }
}

/// DLQ Processor statistics
#[derive(Debug, Clone, Default)]
pub struct DLQStats {
    pub entries_processed: u64,
    pub entries_succeeded: u64,
    pub entries_failed: u64,
    pub entries_skipped: u64,
    pub last_run: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
}

/// Dead Letter Queue Processor
pub struct DLQProcessor {
    config: DLQProcessorConfig,
    transaction_manager: Arc<TransactionManager>,
    handlers: Arc<RwLock<HashMap<String, Arc<dyn DLQHandler>>>>,
    default_handler: Arc<dyn DLQHandler>,
    stats: Arc<RwLock<DLQStats>>,
    running: Arc<std::sync::atomic::AtomicBool>,
}

impl DLQProcessor {
    /// Create a new DLQ processor
    pub fn new(config: DLQProcessorConfig, transaction_manager: Arc<TransactionManager>) -> Self {
        Self {
            config,
            transaction_manager,
            handlers: Arc::new(RwLock::new(HashMap::new())),
            default_handler: Arc::new(LoggingHandler),
            stats: Arc::new(RwLock::new(DLQStats::default())),
            running: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Create with default configuration
    pub fn with_defaults(transaction_manager: Arc<TransactionManager>) -> Self {
        Self::new(DLQProcessorConfig::default(), transaction_manager)
    }

    /// Register a handler for specific operation types
    pub async fn register_handler(&self, handler: Arc<dyn DLQHandler>) {
        let mut handlers = self.handlers.write().await;
        for op_type in handler.supported_types() {
            handlers.insert(op_type.to_string(), handler.clone());
        }
    }

    /// Get handler for an operation type
    async fn get_handler(&self, operation_type: &str) -> Arc<dyn DLQHandler> {
        let handlers = self.handlers.read().await;

        // Try exact match first
        if let Some(handler) = handlers.get(operation_type) {
            return handler.clone();
        }

        // Try wildcard handler
        if let Some(handler) = handlers.get("*") {
            return handler.clone();
        }

        // Fall back to default
        self.default_handler.clone()
    }

    /// Process a single DLQ entry
    async fn process_entry(&self, id: i64, entry: &DLQEntry) -> bool {
        let handler = self.get_handler(&entry.operation_type).await;

        debug!(
            "Processing DLQ entry {} (type: {})",
            id, entry.operation_type
        );

        match handler.process(entry).await {
            DLQResult::Success => {
                if let Err(e) = self
                    .transaction_manager
                    .resolve_dlq(id, "dlq_processor")
                    .await
                {
                    error!("Failed to resolve DLQ entry {}: {}", id, e);
                    return false;
                }

                info!(
                    "DLQ entry {} resolved successfully (type: {})",
                    id, entry.operation_type
                );

                let mut stats = self.stats.write().await;
                stats.entries_succeeded += 1;
                true
            }

            DLQResult::Retry { error } => {
                warn!(
                    "DLQ entry {} will be retried: {} (type: {})",
                    id, error, entry.operation_type
                );

                if let Err(e) = self.transaction_manager.increment_dlq_retry(id).await {
                    error!("Failed to increment DLQ retry count for {}: {}", id, e);
                }

                let mut stats = self.stats.write().await;
                stats.entries_failed += 1;
                stats.last_error = Some(error);
                false
            }

            DLQResult::PermanentFailure { error } => {
                error!(
                    "DLQ entry {} permanently failed: {} (type: {})",
                    id, error, entry.operation_type
                );

                if let Err(e) = self
                    .transaction_manager
                    .mark_dlq_permanent_failure(id, &error)
                    .await
                {
                    error!(
                        "Failed to mark DLQ entry {} as permanently failed: {}",
                        id, e
                    );
                }

                let mut stats = self.stats.write().await;
                stats.entries_failed += 1;
                stats.last_error = Some(error);
                false
            }

            DLQResult::Skip => {
                debug!("DLQ entry {} skipped (type: {})", id, entry.operation_type);

                let mut stats = self.stats.write().await;
                stats.entries_skipped += 1;
                false
            }
        }
    }

    /// Run a single processing cycle
    pub async fn process_cycle(&self) -> crate::error::Result<(u64, u64)> {
        let entries = self
            .transaction_manager
            .get_pending_dlq(self.config.batch_size)
            .await?;

        if entries.is_empty() {
            return Ok((0, 0));
        }

        let mut processed = 0u64;
        let mut succeeded = 0u64;

        for (id, entry) in entries {
            processed += 1;
            if self.process_entry(id, &entry).await {
                succeeded += 1;
            }
        }

        let mut stats = self.stats.write().await;
        stats.entries_processed += processed;
        stats.last_run = Some(chrono::Utc::now());

        info!("DLQ cycle complete: {}/{} succeeded", succeeded, processed);

        Ok((processed, succeeded))
    }

    /// Start the DLQ processor daemon
    pub async fn start(&self) {
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);
        info!(
            "DLQ processor started (interval: {}s, batch: {})",
            self.config.process_interval_secs, self.config.batch_size
        );

        let interval = Duration::from_secs(self.config.process_interval_secs);
        let running = self.running.clone();
        let tm = self.transaction_manager.clone();
        let handlers = self.handlers.clone();
        let default_handler = self.default_handler.clone();
        let stats = self.stats.clone();
        let batch_size = self.config.batch_size;

        tokio::spawn(async move {
            let mut timer = tokio::time::interval(interval);

            while running.load(std::sync::atomic::Ordering::SeqCst) {
                timer.tick().await;

                match tm.get_pending_dlq(batch_size).await {
                    Ok(entries) => {
                        if entries.is_empty() {
                            continue;
                        }

                        let mut processed = 0u64;
                        let mut succeeded = 0u64;

                        for (id, entry) in entries {
                            processed += 1;

                            // Get handler
                            let handler = {
                                let handlers = handlers.read().await;
                                handlers
                                    .get(&entry.operation_type)
                                    .or_else(|| handlers.get("*"))
                                    .cloned()
                                    .unwrap_or_else(|| default_handler.clone())
                            };

                            // Process
                            match handler.process(&entry).await {
                                DLQResult::Success => {
                                    if let Err(e) = tm.resolve_dlq(id, "dlq_processor").await {
                                        error!("Failed to resolve DLQ {}: {}", id, e);
                                    } else {
                                        succeeded += 1;
                                    }
                                }
                                DLQResult::Retry { error } => {
                                    warn!("DLQ {} retry: {}", id, error);
                                    let _ = tm.increment_dlq_retry(id).await;
                                }
                                DLQResult::PermanentFailure { error } => {
                                    error!("DLQ {} permanent failure: {}", id, error);
                                    let _ = tm.mark_dlq_permanent_failure(id, &error).await;
                                }
                                DLQResult::Skip => {
                                    debug!("DLQ {} skipped", id);
                                }
                            }
                        }

                        let mut s = stats.write().await;
                        s.entries_processed += processed;
                        s.entries_succeeded += succeeded;
                        s.last_run = Some(chrono::Utc::now());
                    }
                    Err(e) => {
                        error!("Failed to fetch DLQ entries: {}", e);
                        let mut s = stats.write().await;
                        s.last_error = Some(e.to_string());
                    }
                }
            }

            info!("DLQ processor stopped");
        });
    }

    /// Stop the DLQ processor daemon
    pub fn stop(&self) {
        self.running
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    /// Get current statistics
    pub async fn get_stats(&self) -> DLQStats {
        self.stats.read().await.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_calculation() {
        let config = DLQProcessorConfig {
            base_backoff_secs: 1,
            max_backoff_secs: 60,
            ..Default::default()
        };

        assert_eq!(config.backoff_duration(0), Duration::from_secs(1));
        assert_eq!(config.backoff_duration(1), Duration::from_secs(2));
        assert_eq!(config.backoff_duration(2), Duration::from_secs(4));
        assert_eq!(config.backoff_duration(5), Duration::from_secs(32));
        assert_eq!(config.backoff_duration(6), Duration::from_secs(60)); // capped
    }

    #[test]
    fn test_logging_handler_types() {
        let handler = LoggingHandler;
        assert_eq!(handler.supported_types(), vec!["*"]);
    }
}
