pub mod account;
pub mod adapters;
pub mod agents;
pub mod ai_clients;
#[cfg(feature = "analysis_tools")]
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
pub mod plugins;
pub mod safety;
pub mod signing;
pub mod strategy;
pub mod tui;

// Reinforcement Learning module (optional, enabled with "rl" feature)
#[cfg(feature = "rl")]
pub mod rl;

pub use ai_clients::{AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
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
    AgentStatus, DeploymentState, Domain, ExecutionReport, IntentPurpose, MarketSelector,
    OrderCommand, OrderExecutionReport, OrderIntent, RiskDecision, RiskDecisionStatus, RiskGate,
    StrategyDeployment, Timeframe, TradeIntent,
};
pub use signing::Wallet;
// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{RLConfig, RLStrategy};
