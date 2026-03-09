//! Order Platform - 統一下單平台
//!
//! 提供領域無關的訂單執行、風控和倉位管理。
//! 所有策略 Agent 透過這個平台提交訂單。

mod contracts;
pub mod data_plane;
pub mod freshness;
pub mod legacy_runtime;
pub mod persistence_pipeline;
pub mod persistence_schema;
mod platform;
mod position;
mod queue;
mod risk;
mod router;
pub mod subscription_planner;
mod traits;
mod types;

pub use contracts::{
    DeploymentExecutionMode, MarketSelector, OrderCommand, OrderExecutionReport, RiskDecision,
    RiskDecisionStatus, StrategyDeployment, StrategyEvaluationEvidence, StrategyEvaluationMetrics,
    StrategyEvaluationStage, StrategyLifecycleStage, StrategyProductType, Timeframe, TradeIntent,
};
pub use data_plane::{
    BinanceDataPlaneHandle, CryptoDataPlaneHandle, DataPlaneConfig, DataPlaneHealth,
    PlatformDataPlane, SourceHealth,
};
pub use freshness::{DataPlaneFreshness, DataSource};
pub use persistence_pipeline::{
    BinanceLobTick, BinancePriceTick, ChainlinkPriceTick, ClobOrderbookSnapshot, ClobQuoteTick,
    PersistenceConfig, PersistenceEvent, PersistencePipeline, PersistencePipelineHandle,
    PipelineStats,
};
pub use platform::PlatformStats;
pub use position::{AgentPositionStats, AggregatedPosition, Position, PositionAggregator};
pub use queue::{OrderQueue, QueueStats};
pub use risk::{
    BlockReason, CircuitBreakerEvent, DrawdownSnapshot, PlatformRiskState, RiskCheckResult,
    RiskConfig, RiskGate,
};
pub use router::RouterStats;
pub use subscription_planner::{
    ConsumerId, PlanDelta, SubscriptionKey, SubscriptionPlan, SubscriptionPlanner,
};
pub use traits::{AgentHealthStatus, AgentRiskParams, AgentStatus};
pub use types::{
    CryptoEvent, Domain, DomainEvent, ExecutionReport, ExecutionStatus, OrderIntent, OrderPriority,
    OrderUpdateEvent, PoliticsEvent, QuoteData, QuoteUpdateEvent, SportsEvent,
};
