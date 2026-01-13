pub mod adapters;
pub mod agent;
pub mod api;
pub mod cli;
pub mod collector;
pub mod config;
pub mod coordination;
pub mod domain;
pub mod error;
pub mod platform;
pub mod services;
pub mod signing;
pub mod strategy;
pub mod supervisor;
pub mod persistence;
pub mod tui;
pub mod validation;

// Reinforcement Learning module (optional, enabled with "rl" feature)
#[cfg(feature = "rl")]
pub mod rl;

pub use agent::{AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
pub use collector::{BinanceDepthStream, LobCache, LobSnapshot, SyncCollector, SyncCollectorConfig};
pub use config::AppConfig;
pub use error::{PloyError, Result};
pub use platform::{
    OrderPlatform, PlatformConfig, DomainAgent, AgentStatus, Domain,
    OrderIntent, ExecutionReport, RiskGate, EventRouter,
};
pub use signing::Wallet;
pub use coordination::{
    TradingCircuitBreaker, TradingCircuitBreakerConfig, CircuitState,
    ComponentState, LifecycleManager, LifecycleEvent,
    GracefulShutdown, ShutdownSignal,
};
pub use supervisor::{
    AlertLevel, AlertManager, AlertManagerConfig,
    RecoveryAction, RecoveryPlaybook,
    Watchdog, WatchdogConfig, ComponentHealth,
};
pub use persistence::{
    CheckpointService, CheckpointConfig, Checkpointable,
    DLQProcessor, DLQProcessorConfig, DLQHandler,
    EventStore, StoredEvent, EventMetadata,
};

// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{RLConfig, RLStrategy};
