pub mod fills;
pub mod intents;
pub mod orders;
pub mod pnl;
pub mod positions;
pub mod risk;
pub mod runtime;

pub use fills::{FillLedger, FillRecord};
pub use intents::{IntentPurpose, TradeSide, TradingIntent};
pub use orders::{OrderLedger, OrderRecord, OrderState};
pub use pnl::PnlSnapshot;
pub use positions::{PositionLedger, PositionSnapshot};
pub use risk::{snapshot_from_state, snapshot_from_state_with_prices, RiskSnapshot};
pub use runtime::{TradeCashflowSummary, TradingRuntime, TradingRuntimeSnapshot};

pub const CRATE_MARKER: &str = "ploy-trading";

pub fn crate_marker() -> &'static str {
    CRATE_MARKER
}
