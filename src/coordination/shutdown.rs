//! Graceful Shutdown Handler
//!
//! Provides coordinated shutdown with proper draining of pending operations,
//! replacing forceful aborts with a 120-second timeout and proper sequencing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};
use tracing::{debug, error, info, warn};

/// Shutdown signal types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownSignal {
    /// Normal graceful shutdown (SIGTERM, SIGINT)
    Graceful,
    /// Urgent shutdown - reduce timeouts
    Urgent,
    /// Emergency shutdown - immediate stop
    Emergency,
}

impl std::fmt::Display for ShutdownSignal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownSignal::Graceful => write!(f, "graceful"),
            ShutdownSignal::Urgent => write!(f, "urgent"),
            ShutdownSignal::Emergency => write!(f, "emergency"),
        }
    }
}

/// Configuration for graceful shutdown
#[derive(Debug, Clone)]
pub struct ShutdownConfig {
    /// Total timeout for graceful shutdown (default: 120s)
    pub total_timeout_secs: u64,
    /// Time to wait for pending orders to complete (default: 60s)
    pub order_drain_timeout_secs: u64,
    /// Time to wait for WebSocket cleanup (default: 10s)
    pub websocket_close_timeout_secs: u64,
    /// Time to wait for database flush (default: 30s)
    pub database_flush_timeout_secs: u64,
    /// Poll interval when waiting for pending operations (default: 500ms)
    pub poll_interval_ms: u64,
}

impl Default for ShutdownConfig {
    fn default() -> Self {
        Self {
            total_timeout_secs: 120,
            order_drain_timeout_secs: 60,
            websocket_close_timeout_secs: 10,
            database_flush_timeout_secs: 30,
            poll_interval_ms: 500,
        }
    }
}

/// Shutdown phase tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownPhase {
    /// Not shutting down
    Running,
    /// Stopping new order acceptance
    StoppingNewOrders,
    /// Draining pending orders
    DrainingOrders,
    /// Creating final checkpoint
    Checkpointing,
    /// Closing WebSocket connections
    ClosingWebSockets,
    /// Flushing database
    FlushingDatabase,
    /// Closing database connections
    ClosingConnections,
    /// Shutdown complete
    Complete,
}

impl std::fmt::Display for ShutdownPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownPhase::Running => write!(f, "running"),
            ShutdownPhase::StoppingNewOrders => write!(f, "stopping_new_orders"),
            ShutdownPhase::DrainingOrders => write!(f, "draining_orders"),
            ShutdownPhase::Checkpointing => write!(f, "checkpointing"),
            ShutdownPhase::ClosingWebSockets => write!(f, "closing_websockets"),
            ShutdownPhase::FlushingDatabase => write!(f, "flushing_database"),
            ShutdownPhase::ClosingConnections => write!(f, "closing_connections"),
            ShutdownPhase::Complete => write!(f, "complete"),
        }
    }
}

/// Graceful shutdown coordinator
pub struct GracefulShutdown {
    config: ShutdownConfig,
    shutdown_requested: AtomicBool,
    phase: Arc<watch::Sender<ShutdownPhase>>,
    phase_rx: watch::Receiver<ShutdownPhase>,
    signal_tx: broadcast::Sender<ShutdownSignal>,
    completion_tx: mpsc::Sender<()>,
    completion_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<()>>>,
}

impl GracefulShutdown {
    /// Create a new graceful shutdown handler
    pub fn new(config: ShutdownConfig) -> Self {
        let (phase_tx, phase_rx) = watch::channel(ShutdownPhase::Running);
        let (signal_tx, _) = broadcast::channel(8);
        let (completion_tx, completion_rx) = mpsc::channel(1);

        Self {
            config,
            shutdown_requested: AtomicBool::new(false),
            phase: Arc::new(phase_tx),
            phase_rx,
            signal_tx,
            completion_tx,
            completion_rx: Arc::new(tokio::sync::Mutex::new(completion_rx)),
        }
    }

    /// Create with default configuration
    pub fn with_defaults() -> Self {
        Self::new(ShutdownConfig::default())
    }

    /// Subscribe to shutdown signals
    pub fn subscribe(&self) -> broadcast::Receiver<ShutdownSignal> {
        self.signal_tx.subscribe()
    }

    /// Get a receiver for phase changes
    pub fn phase_receiver(&self) -> watch::Receiver<ShutdownPhase> {
        self.phase_rx.clone()
    }

    /// Check if shutdown has been requested
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Get current shutdown phase
    pub fn current_phase(&self) -> ShutdownPhase {
        *self.phase_rx.borrow()
    }

    /// Request shutdown with specified signal type
    pub fn request_shutdown(&self, signal: ShutdownSignal) {
        if self.shutdown_requested.swap(true, Ordering::SeqCst) {
            warn!("Shutdown already requested, ignoring duplicate signal: {}", signal);
            return;
        }

        info!("Shutdown requested: {}", signal);
        let _ = self.signal_tx.send(signal);
    }

    /// Set current phase
    fn set_phase(&self, phase: ShutdownPhase) {
        let _ = self.phase.send(phase);
        info!("Shutdown phase: {}", phase);
    }

    /// Execute graceful shutdown sequence
    ///
    /// This method orchestrates the entire shutdown process:
    /// 1. Stop accepting new orders
    /// 2. Wait for pending orders to complete
    /// 3. Create final state checkpoint
    /// 4. Close WebSocket connections
    /// 5. Flush database operations
    /// 6. Close database connections
    pub async fn execute<F1, F2, F3, F4, F5>(
        &self,
        stop_new_orders: F1,
        drain_orders: F2,
        checkpoint: F3,
        close_websockets: F4,
        flush_database: F5,
    ) -> Result<(), ShutdownError>
    where
        F1: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
        F2: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>,
        F3: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>,
        F4: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>,
        F5: FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>,
    {
        let start = std::time::Instant::now();
        let total_timeout = Duration::from_secs(self.config.total_timeout_secs);

        info!(
            "Starting graceful shutdown (timeout: {}s)",
            self.config.total_timeout_secs
        );

        // Phase 1: Stop new orders
        self.set_phase(ShutdownPhase::StoppingNewOrders);
        stop_new_orders().await;
        debug!("New order acceptance stopped");

        // Phase 2: Drain pending orders
        self.set_phase(ShutdownPhase::DrainingOrders);
        let drain_timeout = Duration::from_secs(self.config.order_drain_timeout_secs);

        match tokio::time::timeout(drain_timeout, drain_orders()).await {
            Ok(drained) => {
                if drained {
                    info!("All pending orders drained successfully");
                } else {
                    warn!("Some orders may not have completed during drain");
                }
            }
            Err(_) => {
                warn!(
                    "Order drain timeout after {}s, proceeding anyway",
                    self.config.order_drain_timeout_secs
                );
            }
        }

        // Check total timeout
        if start.elapsed() > total_timeout {
            error!("Total shutdown timeout exceeded");
            self.set_phase(ShutdownPhase::Complete);
            return Err(ShutdownError::Timeout);
        }

        // Phase 3: Checkpoint
        self.set_phase(ShutdownPhase::Checkpointing);
        match checkpoint().await {
            Ok(()) => debug!("Final checkpoint created"),
            Err(e) => warn!("Checkpoint failed: {}", e),
        }

        // Phase 4: Close WebSockets
        self.set_phase(ShutdownPhase::ClosingWebSockets);
        let ws_timeout = Duration::from_secs(self.config.websocket_close_timeout_secs);

        match tokio::time::timeout(ws_timeout, close_websockets()).await {
            Ok(()) => debug!("WebSocket connections closed"),
            Err(_) => warn!(
                "WebSocket close timeout after {}s",
                self.config.websocket_close_timeout_secs
            ),
        }

        // Phase 5: Flush database
        self.set_phase(ShutdownPhase::FlushingDatabase);
        let db_timeout = Duration::from_secs(self.config.database_flush_timeout_secs);

        match tokio::time::timeout(db_timeout, flush_database()).await {
            Ok(Ok(())) => debug!("Database flushed successfully"),
            Ok(Err(e)) => warn!("Database flush error: {}", e),
            Err(_) => warn!(
                "Database flush timeout after {}s",
                self.config.database_flush_timeout_secs
            ),
        }

        // Phase 6: Close connections
        self.set_phase(ShutdownPhase::ClosingConnections);
        // Connection closing is typically handled by dropping pool

        // Complete
        self.set_phase(ShutdownPhase::Complete);

        let elapsed = start.elapsed();
        info!("Graceful shutdown completed in {:?}", elapsed);

        // Signal completion
        let _ = self.completion_tx.send(()).await;

        Ok(())
    }

    /// Wait for shutdown to complete
    pub async fn wait_for_completion(&self) {
        let mut rx = self.completion_rx.lock().await;
        let _ = rx.recv().await;
    }

    /// Create a token that can be used to check shutdown status
    pub fn token(&self) -> ShutdownToken {
        ShutdownToken {
            shutdown_requested: self.shutdown_requested.load(Ordering::SeqCst),
            signal_rx: self.signal_tx.subscribe(),
            phase_rx: self.phase_rx.clone(),
        }
    }
}

/// Token for checking shutdown status in async tasks
pub struct ShutdownToken {
    shutdown_requested: bool,
    signal_rx: broadcast::Receiver<ShutdownSignal>,
    phase_rx: watch::Receiver<ShutdownPhase>,
}

impl ShutdownToken {
    /// Check if shutdown was requested at token creation time
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested
    }

    /// Wait for shutdown signal
    pub async fn wait_for_shutdown(&mut self) -> ShutdownSignal {
        match self.signal_rx.recv().await {
            Ok(signal) => signal,
            Err(_) => ShutdownSignal::Emergency, // Channel closed = emergency
        }
    }

    /// Get current phase
    pub fn current_phase(&self) -> ShutdownPhase {
        *self.phase_rx.borrow()
    }

    /// Wait for specific phase
    pub async fn wait_for_phase(&mut self, target: ShutdownPhase) {
        while *self.phase_rx.borrow() != target {
            if self.phase_rx.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Shutdown errors
#[derive(Debug, Clone)]
pub enum ShutdownError {
    /// Shutdown timed out
    Timeout,
    /// Shutdown was interrupted
    Interrupted,
    /// Component failed during shutdown
    ComponentFailed(String),
}

impl std::fmt::Display for ShutdownError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownError::Timeout => write!(f, "shutdown timed out"),
            ShutdownError::Interrupted => write!(f, "shutdown interrupted"),
            ShutdownError::ComponentFailed(c) => write!(f, "component {} failed during shutdown", c),
        }
    }
}

impl std::error::Error for ShutdownError {}

/// Helper to install OS signal handlers
pub async fn install_signal_handlers(shutdown: Arc<GracefulShutdown>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let shutdown_sigterm = shutdown.clone();
        let shutdown_sigint = shutdown.clone();
        let shutdown_sigquit = shutdown.clone();

        // Handle SIGTERM
        tokio::spawn(async move {
            let mut stream = signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");
            stream.recv().await;
            info!("Received SIGTERM");
            shutdown_sigterm.request_shutdown(ShutdownSignal::Graceful);
        });

        // Handle SIGINT (Ctrl+C)
        tokio::spawn(async move {
            let mut stream = signal(SignalKind::interrupt()).expect("Failed to install SIGINT handler");
            stream.recv().await;
            info!("Received SIGINT");
            shutdown_sigint.request_shutdown(ShutdownSignal::Graceful);
        });

        // Handle SIGQUIT (Ctrl+\)
        tokio::spawn(async move {
            let mut stream = signal(SignalKind::quit()).expect("Failed to install SIGQUIT handler");
            stream.recv().await;
            warn!("Received SIGQUIT - urgent shutdown");
            shutdown_sigquit.request_shutdown(ShutdownSignal::Urgent);
        });
    }

    #[cfg(windows)]
    {
        let shutdown_ctrl_c = shutdown.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.expect("Failed to install Ctrl+C handler");
            info!("Received Ctrl+C");
            shutdown_ctrl_c.request_shutdown(ShutdownSignal::Graceful);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shutdown_signal_display() {
        assert_eq!(ShutdownSignal::Graceful.to_string(), "graceful");
        assert_eq!(ShutdownSignal::Urgent.to_string(), "urgent");
        assert_eq!(ShutdownSignal::Emergency.to_string(), "emergency");
    }

    #[test]
    fn test_shutdown_phase_display() {
        assert_eq!(ShutdownPhase::Running.to_string(), "running");
        assert_eq!(ShutdownPhase::DrainingOrders.to_string(), "draining_orders");
        assert_eq!(ShutdownPhase::Complete.to_string(), "complete");
    }

    #[tokio::test]
    async fn test_shutdown_request() {
        let shutdown = GracefulShutdown::with_defaults();

        assert!(!shutdown.is_shutdown_requested());
        assert_eq!(shutdown.current_phase(), ShutdownPhase::Running);

        shutdown.request_shutdown(ShutdownSignal::Graceful);
        assert!(shutdown.is_shutdown_requested());

        // Duplicate request should be ignored
        shutdown.request_shutdown(ShutdownSignal::Urgent);
        assert!(shutdown.is_shutdown_requested());
    }

    #[tokio::test]
    async fn test_shutdown_token() {
        let shutdown = GracefulShutdown::with_defaults();
        let token = shutdown.token();

        assert!(!token.is_shutdown_requested());
        assert_eq!(token.current_phase(), ShutdownPhase::Running);
    }
}
