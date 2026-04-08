pub mod agent;
pub mod audit;
pub mod deployments;
pub mod diagnostics;
pub mod errors;
pub mod events;
pub mod oversight;
pub mod proposals;
pub mod system;
pub mod trading;

pub use agent::{
    AgentRunEvaluation, AgentRunOutputSummary, AgentRunRecord, AgentRunStatus,
    AgentRuntimeContextSummary, AgentToolCallRecord,
};
pub use audit::AuditLogEntry;
pub use deployments::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DeploymentStateSummary,
    DeploymentSummary, DesiredState, ObservedState,
};
pub use diagnostics::{
    DeploymentDiagnosticsMetrics, DeploymentDiagnosticsReport, DiagnosticsEvidence,
    DiagnosticsFinding, PlatformDiagnosticsReport,
};
pub use errors::ControlPlaneErrorResponse;
pub use events::{
    AlertSnapshotEvent, DeploymentSnapshotEvent, LogEntry, MetricsSnapshotEvent, OperatorEvent,
    StatusUpdate, SystemSnapshotEvent, TradingSnapshotEvent, WsMessage,
};
pub use oversight::{
    build_operator_command, build_oversight_actions, collect_oversight_signals,
    compute_oversight_report, OversightAction, OversightReport, OversightSignal,
    OversightSnapshotEvent,
};
pub use proposals::{
    ProposalActionKind, ProposalCreateRequest, ProposalDecisionRequest, ProposalSnapshotEvent,
    ProposalStatus, SafetyProposal,
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
