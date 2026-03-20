pub mod deployments;
pub mod errors;
pub mod events;
pub mod system;
pub mod trading;

pub use deployments::{
    DeploymentApplyRequest, DeploymentControlRequest, DeploymentState, DeploymentStateSummary,
    DeploymentSummary, DesiredState, ObservedState,
};
pub use errors::ControlPlaneErrorResponse;
pub use events::{
    DeploymentSnapshotEvent, LogEntry, OperatorEvent, StatusUpdate, SystemSnapshotEvent,
    TradingSnapshotEvent, WsMessage,
};
pub use system::{SystemControlResponse, SystemStatus};
pub use trading::{
    FillSnapshot, IntentPurpose, MarketData, OrderSnapshot, PaperIntentRequest,
    PaperIntentResponse, PnlSnapshotResponse, PositionResponse, PositionSnapshotResponse,
    RiskSnapshotResponse, TradeResponse, TradingIntentSnapshot, TradingStateSnapshot,
};

pub const CRATE_MARKER: &str = "ploy-operator-contracts";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
