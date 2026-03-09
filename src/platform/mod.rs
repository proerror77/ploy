//! Order Platform - legacy execution surface
//!
//! 提供舊版 queue/risk/execution 元件給 RL CLI 等兼容層使用。
//! 正式 live trading runtime 已由 coordinator 接管。

mod contracts;
pub mod data_plane;
pub mod freshness;
pub mod persistence_pipeline;
pub mod persistence_schema;
mod platform;
mod position;
mod queue;
mod risk;
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
pub use platform::{OrderPlatform, PlatformConfig, PlatformStats};
pub use position::{AgentPositionStats, AggregatedPosition, Position, PositionAggregator};
pub use queue::{OrderQueue, QueueStats};
pub use risk::{
    BlockReason, CircuitBreakerEvent, DrawdownSnapshot, PlatformRiskState, RiskCheckResult,
    RiskConfig, RiskGate,
};
pub use subscription_planner::{
    ConsumerId, PlanDelta, SubscriptionKey, SubscriptionPlan, SubscriptionPlanner,
};
pub use traits::{AgentRiskParams, AgentStatus};
pub use types::{
    CryptoEvent, Domain, DomainEvent, ExecutionReport, ExecutionStatus, OrderIntent, OrderPriority,
    OrderUpdateEvent, PoliticsEvent, QuoteData, QuoteUpdateEvent, SportsEvent,
};
