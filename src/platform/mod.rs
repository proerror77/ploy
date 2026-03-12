//! Order Platform - legacy execution surface
//!
//! 提供舊版 queue/risk/execution 元件給 RL CLI 等兼容層使用。
//! 正式 live trading runtime 已由 coordinator 接管。

pub mod data_plane;
pub mod freshness;
mod market_persistence;
pub mod persistence_pipeline;
pub mod persistence_schema;
pub mod subscription_planner;
pub mod types;

pub use data_plane::{
    BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig, DataPlaneHealth,
    PlatformDataPlane, SourceHealth,
};
pub use freshness::{DataPlaneFreshness, DataSource};
pub(crate) use market_persistence::{
    ensure_clob_trade_alerts_table, spawn_pm_token_settlement_persistence,
    spawn_polymarket_trade_persistence, spawn_polymarket_trade_persistence_from_collector_targets,
};
pub use persistence_pipeline::{
    BinanceLobTick, BinancePriceTick, ChainlinkPriceTick, ClobOrderbookSnapshot,
    ClobQuoteTick, PersistenceConfig, PersistenceEvent,
    PersistencePipeline, PersistencePipelineHandle, PipelineStats,
};
pub use subscription_planner::{
    ConsumerId, PlanDelta, SubscriptionKey, SubscriptionPlan, SubscriptionPlanner,
};
pub use types::{Domain, IntentPurpose, OrderIntent, OrderPriority};

// Re-exports for backward-compat: types that moved to other modules but are
// still referenced as `crate::platform::*` throughout the codebase.
pub use crate::agent_runtime::{AgentRiskParams, AgentStatus};
pub use crate::control_plane::{
    DeploymentExecutionMode, MarketSelector, StrategyDeployment, StrategyLifecycleStage,
    StrategyProductType,
};
pub use crate::coordinator::{BlockReason, PlatformRiskState, RiskCheckResult, RiskConfig, RiskGate};
pub use crate::plugins::DeploymentState;
