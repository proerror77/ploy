#![allow(
    async_fn_in_trait,
    dead_code,
    clippy::clone_on_copy,
    clippy::collapsible_else_if,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::assign_op_pattern,
    clippy::cast_abs_to_unsigned,
    clippy::derivable_impls,
    clippy::doc_overindented_list_items,
    clippy::double_ended_iterator_last,
    clippy::excessive_precision,
    clippy::explicit_auto_deref,
    clippy::field_reassign_with_default,
    clippy::for_kv_map,
    clippy::format_in_format_args,
    clippy::if_same_then_else,
    clippy::iter_cloned_collect,
    clippy::large_enum_variant,
    clippy::manual_contains,
    clippy::manual_async_fn,
    clippy::manual_clamp,
    clippy::manual_is_multiple_of,
    clippy::map_flatten,
    clippy::module_inception,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_lifetimes,
    clippy::needless_range_loop,
    clippy::new_without_default,
    clippy::print_literal,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::redundant_locals,
    clippy::result_large_err,
    clippy::search_is_some,
    clippy::should_implement_trait,
    clippy::to_string_in_format_args,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_cast,
    clippy::unnecessary_lazy_evaluations,
    clippy::unnecessary_map_or,
    clippy::unnecessary_min_or_max,
    clippy::unwrap_or_default,
    clippy::useless_conversion,
    clippy::useless_asref,
    clippy::write_literal,
    clippy::write_with_newline,
    clippy::wrong_self_convention
)]

pub mod account;
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
pub mod plugins;
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
pub use coordinator::OrderIntent;
pub use coordinator::RiskGate;
pub use domain::Domain;
pub use error::{PloyError, Result};
pub use persistence::{
    CheckpointConfig, CheckpointService, Checkpointable, DLQHandler, DLQProcessor,
    DLQProcessorConfig, EventMetadata, EventStore, StoredEvent,
};
pub use signing::Wallet;
pub use supervisor::{
    AlertLevel, AlertManager, AlertManagerConfig, ComponentHealth, RecoveryAction,
    RecoveryPlaybook, Watchdog, WatchdogConfig,
};

// RL exports (when feature enabled)
#[cfg(feature = "rl")]
pub use rl::{ExecutionReport, ExecutionStatus, RLConfig, RLStrategy};
