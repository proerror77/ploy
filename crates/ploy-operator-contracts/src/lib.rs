pub mod audit;
pub mod diagnostics;
pub mod deployments;
pub mod errors;
pub mod events;
pub mod system;
pub mod trading;

pub use audit::AuditLogEntry;
pub use diagnostics::{
    AgentRunRecord, DeploymentDiagnosticsMetrics, DeploymentDiagnosticsReport,
    DiagnosticsEvidence, DiagnosticsFinding, OversightRecommendedAction, OversightReport,
    OversightSignal, OversightSnapshotEvent, PlatformDiagnosticsReport, ProposalActionKind,
    ProposalCreateRequest, ProposalDecisionRequest, ProposalSnapshotEvent, ProposalStatus,
    SafetyProposal, compute_oversight_report,
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
pub use system::{
    ActiveAlert, AlertKind, AlertSeverity, HeartbeatState, HeartbeatStatus, PlatformMetrics,
    SystemControlResponse, SystemStatus,
};
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
