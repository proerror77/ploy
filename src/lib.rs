pub mod adapters;
pub mod agent;
pub mod agent_system;
pub mod agents;
pub mod analysis;
#[cfg(feature = "api")]
pub mod api;
pub mod cli;
pub mod collector;
pub mod config;
pub mod coordination;
pub mod coordinator;
pub mod domain;
pub mod error;
pub mod exchange;
pub mod ml;
pub mod persistence;
pub mod platform;
pub mod safety;
pub mod services;
pub mod signing;
pub mod strategy;
pub mod supervisor;
pub mod tui;
pub mod validation;

// Reinforcement Learning module (optional, enabled with "rl" feature)
#[cfg(feature = "rl")]
pub mod rl;

pub use agent::{AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
pub use collector::{
    BinanceDepthStream, LobCache, LobSnapshot, SyncCollector, SyncCollectorConfig,
};
pub use config::AppConfig;
pub use coordination::{
    CircuitState, ComponentState, GracefulShutdown, LifecycleEvent, LifecycleManager,
    ShutdownSignal, TradingCircuitBreaker, TradingCircuitBreakerConfig,
};
pub use error::{PloyError, Result};
pub use persistence::{
    CheckpointConfig, CheckpointService, Checkpointable, DLQHandler, DLQProcessor,
    DLQProcessorConfig, EventMetadata, EventStore, StoredEvent,
};
pub use platform::{
    AgentStatus, Domain, DomainAgent, EventRouter, ExecutionReport, MarketSelector, OrderCommand,
    OrderExecutionReport, OrderIntent, OrderPlatform, PlatformConfig, RiskDecision,
    RiskDecisionStatus, RiskGate, StrategyDeployment, Timeframe, TradeIntent,
};
pub use signing::Wallet;
pub use supervisor::{
    AlertLevel, AlertManager, AlertManagerConfig, ComponentHealth, RecoveryAction,
    RecoveryPlaybook, Watchdog, WatchdogConfig,
};

// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{RLConfig, RLStrategy};
