//! Coordination Layer for 24/7 System Operation
//!
//! This module provides coordination infrastructure for reliable system operation:
//! - Lifecycle management for ordered component startup/shutdown
//! - Circuit breaker for trading operations
//! - Backpressure control for quote processing
//! - Graceful shutdown handling

pub mod circuit_breaker;
pub mod lifecycle;
pub mod shutdown;
pub mod emergency_stop;

pub use circuit_breaker::{TradingCircuitBreaker, TradingCircuitBreakerConfig, CircuitState};
pub use lifecycle::{ComponentState, LifecycleManager, LifecycleEvent};
pub use shutdown::{GracefulShutdown, ShutdownSignal};
pub use emergency_stop::{
    EmergencyStopManager, EmergencyStopConfig, EmergencyState, EmergencyReason,
};
