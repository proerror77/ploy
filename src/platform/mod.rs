//! Order Platform - 統一下單平台
//!
//! 提供領域無關的訂單執行、風控和倉位管理。
//! Canonical live strategies now flow through coordinator-managed runtime paths.
//! This module keeps shared order-plane contracts, data-plane utilities, and
//! risk primitives used by the coordinator and command helpers.

mod contracts;
pub mod data_plane;
pub mod freshness;
pub mod persistence_pipeline;
pub mod persistence_schema;
mod position;
mod queue;
mod risk;
pub mod subscription_planner;
pub mod traits;
mod types;

pub use crate::plugins::DeploymentState;
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
    CryptoEvent, Domain, DomainEvent, ExecutionReport, ExecutionStatus, IntentPurpose, OrderIntent,
    OrderPriority, OrderUpdateEvent, PoliticsEvent, QuoteData, QuoteUpdateEvent, SportsEvent,
};
