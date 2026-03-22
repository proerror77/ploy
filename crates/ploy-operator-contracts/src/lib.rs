pub mod alerts;
pub mod audit;
pub mod claims;
pub mod deployments;
pub mod errors;
pub mod events;
pub mod metrics;
pub mod system;
pub mod trading;

pub use alerts::{AlertRecord, AlertSeverity};
pub use audit::AuditLogEntry;
pub use claims::{
    AccountClaimActionResponse, AccountClaimActionState, AccountClaimDetailResponse,
    AccountClaimStatus, ClaimExecutionOutcome, ClaimExecutionRecord, ClaimLoopState,
    ClaimPositionState, RedeemablePositionSnapshot,
};
pub use deployments::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DeploymentStateSummary,
    DeploymentSummary, DesiredState, ObservedState,
};
pub use errors::ControlPlaneErrorResponse;
pub use events::{
    AlertSnapshotEvent, DeploymentSnapshotEvent, LogEntry, MetricsSnapshotEvent, OperatorEvent,
    StatusUpdate, SystemSnapshotEvent, TradingSnapshotEvent, WsMessage,
};
pub use metrics::SystemMetrics;
pub use system::{SystemControlResponse, SystemStatus};
pub use trading::{
    FillSnapshot, IntentPurpose, MarketData, OrderControlResponse, OrderReplaceRequest,
    OrderSnapshot, PaperIntentRequest, PaperIntentResponse, PnlSnapshotResponse, PositionResponse,
    PositionSnapshotResponse, RiskSnapshotResponse, TradeResponse, TradingIntentSnapshot,
    TradingStateSnapshot,
};

pub const CRATE_MARKER: &str = "ploy-operator-contracts";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
