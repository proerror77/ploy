//! Coordination Layer for 24/7 System Operation
//!
//! This module provides coordination infrastructure for reliable system operation:
//! - Lifecycle management for ordered component startup/shutdown
//! - Circuit breaker for trading operations
//! - Backpressure control for quote processing
//! - Graceful shutdown handling

pub mod circuit_breaker;
pub mod emergency_stop;
pub mod lifecycle;
pub mod shutdown;

pub use circuit_breaker::{CircuitState, TradingCircuitBreaker, TradingCircuitBreakerConfig};
pub use emergency_stop::{
    EmergencyReason, EmergencyState, EmergencyStopConfig, EmergencyStopManager,
};
pub use lifecycle::{ComponentState, LifecycleEvent, LifecycleManager};
pub use shutdown::{GracefulShutdown, ShutdownSignal};
