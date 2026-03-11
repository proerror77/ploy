pub mod adapters;
pub mod agent_runtime;
pub mod agents;
pub mod ai_clients;
#[cfg(feature = "analysis_tools")]
pub mod analysis;
#[cfg(feature = "api")]
pub mod api;
pub mod cli;
pub mod collector;
pub mod config;
pub mod control_plane;
pub mod coordination;
pub mod coordinator;
pub mod data_plane;
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

pub use agent_runtime::{AgentRiskParams, AgentStatus};
pub use ai_clients::{AdvisoryAgent, AutonomousAgent, AutonomousConfig, ClaudeAgentClient};
pub use collector::{
    BinanceDepthStream, LobCache, LobSnapshot, SyncCollector, SyncCollectorConfig,
};
pub use config::AppConfig;
pub use control_plane::{
    DeploymentExecutionMode, MarketSelector, RiskDecision, RiskDecisionStatus, StrategyDeployment,
    StrategyEvaluationEvidence, StrategyEvaluationMetrics, StrategyEvaluationStage,
    StrategyLifecycleStage, StrategyProductType, Timeframe, TradeIntent,
};
pub use coordination::{
    CircuitState, ComponentState, GracefulShutdown, LifecycleEvent, LifecycleManager,
    ShutdownSignal, TradingCircuitBreaker, TradingCircuitBreakerConfig,
};
pub use error::{PloyError, Result};
pub use persistence::{
    CheckpointConfig, CheckpointService, Checkpointable, DLQHandler, DLQProcessor,
    DLQProcessorConfig, EventMetadata, EventStore, StoredEvent,
};
pub use coordinator::OrderIntent;
pub use coordinator::RiskGate;
pub use domain::Domain;
pub use signing::Wallet;
pub use supervisor::{
    AlertLevel, AlertManager, AlertManagerConfig, ComponentHealth, RecoveryAction,
    RecoveryPlaybook, Watchdog, WatchdogConfig,
};

// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{ExecutionReport, ExecutionStatus, RLConfig, RLStrategy};
